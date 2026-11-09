//! Host side: accept one viewer at a time, send it the screen and apply its
//! input. Screen capture and input injection are behind [`Platform`], so the
//! session logic runs (and is tested) on any system.

use crate::{
    auth::{self, AccessKey, PeerPolicy},
    codec::{Encoder, Frame},
    proto::{Button, ClientMsg, HostMsg},
};
use std::{
    collections::BTreeSet,
    io,
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// Frames sent but not yet shown by the viewer. With more than this the host
/// waits, so a slow connection lowers the frame rate instead of adding delay.
const FRAMES_IN_FLIGHT: u32 = 2;
/// A still picture is sent again in high quality after this long.
const REFINE_AFTER: Duration = Duration::from_millis(250);
const PING_EVERY: Duration = Duration::from_secs(2);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// A viewer that shows no frame for this long while frames are waiting is gone.
const ACK_TIMEOUT: Duration = Duration::from_secs(15);
/// The viewer's "host default" monitor.
pub const DEFAULT_MONITOR: u8 = 255;

/// The captured monitor's place on the desktop, in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DesktopRect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl DesktopRect {
    /// The desktop pixel for a viewer position (each axis 0..=65535 across the monitor).
    pub fn point(&self, x: u16, y: u16) -> (i32, i32) {
        let along = |position: u16, size: u32| (position as u64 * size.saturating_sub(1) as u64 / 65535) as i32;
        (self.left + along(x, self.width), self.top + along(y, self.height))
    }
}

pub trait Capturer {
    /// Wait up to `timeout` for the screen to change and copy it into `frame`
    /// (resizing it if the monitor's mode changed). `Ok(false)`: no new picture.
    fn capture(&mut self, timeout: Duration, frame: &mut Frame) -> io::Result<bool>;
    fn desktop_rect(&self) -> DesktopRect;
    /// The monitor in use and how many there are.
    fn monitor(&self) -> (u8, u8);
}

pub trait Injector {
    /// Move the pointer to this desktop pixel.
    fn mouse_move(&mut self, x: i32, y: i32);
    fn button(&mut self, button: Button, down: bool);
    fn wheel(&mut self, dx: i16, dy: i16);
    fn key(&mut self, scancode: u16, down: bool);
}

/// Creates the capturer and injector of a session, each on the thread that uses it.
pub trait Platform: Send + Sync + 'static {
    fn capturer(&self, monitor: u8) -> io::Result<Box<dyn Capturer>>;
    fn injector(&self) -> io::Result<Box<dyn Injector>>;
}

pub type Log = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Clone)]
pub struct HostConfig {
    pub key: AccessKey,
    pub policy: PeerPolicy,
    /// Monitor for viewers that do not ask for one.
    pub monitor: u8,
    pub log: Log,
}

impl std::fmt::Debug for HostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostConfig")
            .field("policy", &self.policy)
            .field("monitor", &self.monitor)
            .finish()
    }
}

/// Delay after failed handshakes: 1 s, 2 s, 4 s, up to 32 s, during which every
/// connection is dropped unanswered. A correct key clears it.
#[derive(Debug)]
struct Throttle {
    failures: u32,
    until: Instant,
}

/// Accept viewers until the listener fails. A newly accepted viewer replaces the current one.
pub fn serve(listener: TcpListener, config: HostConfig, platform: Arc<dyn Platform>) -> io::Result<()> {
    let mut throttle = Throttle {
        failures: 0,
        until: Instant::now(),
    };
    let current: Arc<Mutex<Option<TcpStream>>> = Arc::default();
    loop {
        let (mut stream, peer) = listener.accept()?;
        if !config.policy.allows(peer.ip()) {
            (config.log)(format!("refused {peer}: not a Tailscale address"));
            continue;
        }
        if Instant::now() < throttle.until {
            continue;
        }
        let prepared = stream
            .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(WRITE_TIMEOUT)))
            .and_then(|()| stream.set_nodelay(true));
        match prepared.and_then(|()| auth::host_handshake(&mut stream, &config.key)) {
            Ok(()) => throttle.failures = 0,
            Err(error) => {
                // Every failure counts: a viewer with the wrong key notices first
                // and leaves before the host has seen its proof.
                throttle.failures += 1;
                throttle.until = Instant::now() + Duration::from_secs(1 << (throttle.failures - 1).min(5));
                (config.log)(format!("refused {peer}: {error}"));
                continue;
            }
        }
        let Ok(control) = stream.try_clone() else { continue };
        if let Some(old) = current.lock().unwrap().replace(control) {
            let _ = old.shutdown(Shutdown::Both);
        }
        (config.log)(format!("{peer} connected"));
        let (platform, log, monitor) = (platform.clone(), config.log.clone(), config.monitor);
        thread::spawn(move || {
            let ended = run_session(stream, platform.as_ref(), monitor);
            log(match ended {
                Ok(()) => format!("{peer} disconnected"),
                Err(error) => format!("{peer} disconnected: {error}"),
            });
        });
    }
}

#[derive(Debug, Default)]
struct Flow {
    sent: u32,
    shown: u32,
}

#[derive(Debug, Default)]
struct Shared {
    closed: AtomicBool,
    flow: Mutex<Flow>,
    shown: Condvar,
    rect: Mutex<DesktopRect>,
}

/// Keys and buttons the viewer holds down, released when it leaves.
#[derive(Debug, Default)]
struct Held {
    keys: BTreeSet<u16>,
    buttons: Vec<Button>,
}

impl Held {
    fn release(&mut self, injector: &mut dyn Injector) {
        for scancode in std::mem::take(&mut self.keys) {
            injector.key(scancode, false);
        }
        for button in std::mem::take(&mut self.buttons) {
            injector.button(button, false);
        }
    }
}

fn read_input(mut stream: &TcpStream, shared: &Shared, injector: &mut dyn Injector) -> io::Result<()> {
    let mut held = Held::default();
    let result = loop {
        let message = match ClientMsg::read(&mut stream) {
            Ok(message) => message,
            Err(error) => break Err(error),
        };
        match message {
            ClientMsg::Hello { .. } => break Err(io::Error::new(io::ErrorKind::InvalidData, "second hello")),
            ClientMsg::Ack { seq } => {
                let mut flow = shared.flow.lock().unwrap();
                flow.shown = flow.shown.max(seq.min(flow.sent));
                shared.shown.notify_all();
            }
            ClientMsg::MouseMove { x, y } => {
                let (x, y) = shared.rect.lock().unwrap().point(x, y);
                injector.mouse_move(x, y);
            }
            ClientMsg::MouseButton { button, down } => {
                held.buttons.retain(|held| *held != button);
                if down {
                    held.buttons.push(button);
                }
                injector.button(button, down);
            }
            ClientMsg::Wheel { dx, dy } => injector.wheel(dx, dy),
            ClientMsg::Key { scancode, down } => {
                if down {
                    held.keys.insert(scancode);
                } else {
                    held.keys.remove(&scancode);
                }
                injector.key(scancode, down);
            }
            ClientMsg::ReleaseAll => held.release(injector),
        }
    };
    held.release(injector);
    result
}

fn send_frames(
    mut stream: &TcpStream,
    shared: &Shared,
    capturer: &mut dyn Capturer,
    max_fps: u8,
    quality: u8,
) -> io::Result<()> {
    let interval = Duration::from_secs(1) / max_fps.clamp(1, 120) as u32;
    let mut encoder = Encoder::new();
    let mut frame = Frame::default();
    let mut size = (0, 0);
    let mut last_sent = Instant::now();
    let mut last_change = Instant::now();
    let mut last_capture = Instant::now() - interval;
    while !shared.closed.load(Ordering::Acquire) {
        {
            let mut flow = shared.flow.lock().unwrap();
            let waiting_since = Instant::now();
            while flow.sent - flow.shown >= FRAMES_IN_FLIGHT && !shared.closed.load(Ordering::Acquire) {
                if waiting_since.elapsed() > ACK_TIMEOUT {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "the viewer stopped responding"));
                }
                flow = shared.shown.wait_timeout(flow, Duration::from_millis(100)).unwrap().0;
            }
        }
        thread::sleep(interval.saturating_sub(last_capture.elapsed()));
        last_capture = Instant::now();
        let changed = capturer.capture(Duration::from_millis(100), &mut frame)?;
        let mut rects = Vec::new();
        if changed {
            *shared.rect.lock().unwrap() = capturer.desktop_rect();
            if size != (frame.width, frame.height) {
                size = (frame.width, frame.height);
                let (monitor, monitor_count) = capturer.monitor();
                HostMsg::Screen {
                    width: frame.width,
                    height: frame.height,
                    monitor,
                    monitor_count,
                }
                .write(&mut stream)?;
                encoder.reset();
            }
            rects = encoder.encode(&frame, quality);
            if !rects.is_empty() {
                last_change = Instant::now();
            }
        }
        if rects.is_empty() && encoder.needs_refine() && last_change.elapsed() >= REFINE_AFTER {
            rects = encoder.refine();
        }
        if !rects.is_empty() {
            let seq = {
                let mut flow = shared.flow.lock().unwrap();
                flow.sent += 1;
                flow.sent
            };
            HostMsg::Frame { seq, rects }.write(&mut stream)?;
            last_sent = Instant::now();
        } else if last_sent.elapsed() >= PING_EVERY {
            HostMsg::Ping.write(&mut stream)?;
            last_sent = Instant::now();
        }
    }
    Ok(())
}

/// One authenticated viewer, from its hello to the end of the connection.
pub fn run_session(stream: TcpStream, platform: &dyn Platform, default_monitor: u8) -> io::Result<()> {
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let ClientMsg::Hello {
        monitor,
        max_fps,
        quality,
    } = ClientMsg::read(&mut &stream)?
    else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "expected hello"));
    };
    stream.set_read_timeout(None)?;
    let monitor = if monitor == DEFAULT_MONITOR {
        default_monitor
    } else {
        monitor
    };
    let shared = Shared::default();
    let result = thread::scope(|scope| {
        let input = scope.spawn(|| {
            let result = platform
                .injector()
                .and_then(|mut injector| read_input(&stream, &shared, injector.as_mut()));
            shared.closed.store(true, Ordering::Release);
            shared.shown.notify_all();
            result
        });
        let frames = platform
            .capturer(monitor)
            .and_then(|mut capturer| send_frames(&stream, &shared, capturer.as_mut(), max_fps, quality));
        shared.closed.store(true, Ordering::Release);
        let _ = stream.shutdown(Shutdown::Both);
        let input = input.join().expect("the input thread panicked");
        // Report the side that failed first: the other one only saw the closed connection.
        frames.and(match input {
            Err(error) if error.kind() != io::ErrorKind::UnexpectedEof => Err(error),
            _ => Ok(()),
        })
    });
    match result {
        Err(error) if matches!(error.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset) => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_positions_map_onto_the_monitor() {
        let rect = DesktopRect {
            left: -1920,
            top: 100,
            width: 1920,
            height: 1080,
        };
        assert_eq!(rect.point(0, 0), (-1920, 100));
        assert_eq!(rect.point(65535, 65535), (-1, 1179));
        assert_eq!(rect.point(32768, 32768), (-961, 639));
        assert_eq!(DesktopRect::default().point(65535, 65535), (0, 0));
    }
}
