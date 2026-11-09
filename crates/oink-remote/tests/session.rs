//! A whole session over a local connection, with a scripted screen and a
//! recording injector in place of a real computer.

use oink_remote::{
    auth::{AccessKey, PeerPolicy},
    client::{Client, Event, Options},
    codec::Frame,
    host::{self, Capturer, DesktopRect, HostConfig, Injector, Platform},
    proto::{Button, ClientMsg},
};
use std::{
    io,
    net::TcpListener,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

const CODE: &str = "ABCDE-FGHJK-LMNPQ-RSTUV";

/// Shows each scripted frame once, then reports "no change".
struct Script {
    frames: Arc<Mutex<Vec<Frame>>>,
}

impl Capturer for Script {
    fn capture(&mut self, timeout: Duration, frame: &mut Frame) -> io::Result<bool> {
        let mut frames = self.frames.lock().unwrap();
        if frames.is_empty() {
            drop(frames);
            thread::sleep(timeout.min(Duration::from_millis(5)));
            return Ok(false);
        }
        *frame = frames.remove(0);
        Ok(true)
    }
    fn desktop_rect(&self) -> DesktopRect {
        DesktopRect {
            left: 1000,
            top: 0,
            width: 200,
            height: 100,
        }
    }
    fn monitor(&self) -> (u8, u8) {
        (1, 2)
    }
}

struct Recorder(Arc<Mutex<Vec<String>>>);

impl Injector for Recorder {
    fn mouse_move(&mut self, x: i32, y: i32) {
        self.0.lock().unwrap().push(format!("move {x},{y}"));
    }
    fn button(&mut self, button: Button, down: bool) {
        self.0.lock().unwrap().push(format!("button {button:?} {down}"));
    }
    fn wheel(&mut self, dx: i16, dy: i16) {
        self.0.lock().unwrap().push(format!("wheel {dx},{dy}"));
    }
    fn key(&mut self, scancode: u16, down: bool) {
        self.0.lock().unwrap().push(format!("key {scancode:#x} {down}"));
    }
}

#[derive(Default)]
struct Fake {
    frames: Arc<Mutex<Vec<Frame>>>,
    input: Arc<Mutex<Vec<String>>>,
    monitors: Arc<Mutex<Vec<u8>>>,
}

impl Platform for Fake {
    fn capturer(&self, monitor: u8) -> io::Result<Box<dyn Capturer>> {
        self.monitors.lock().unwrap().push(monitor);
        Ok(Box::new(Script {
            frames: self.frames.clone(),
        }))
    }
    fn injector(&self) -> io::Result<Box<dyn Injector>> {
        Ok(Box::new(Recorder(self.input.clone())))
    }
}

fn solid(width: u32, height: u32, bgra: [u8; 4]) -> Frame {
    let mut frame = Frame::new(width, height);
    for pixel in frame.data.as_chunks_mut::<4>().0 {
        *pixel = bgra;
    }
    frame
}

fn start(fake: Arc<Fake>, policy: PeerPolicy) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let config = HostConfig {
        key: AccessKey::from_code(CODE).unwrap(),
        policy,
        monitor: 0,
        log: Arc::new(|_line| {}),
    };
    thread::spawn(move || host::serve(listener, config, fake));
    address
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out waiting for {what}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn close_to(pixel: u32, rgb: [u8; 3]) -> bool {
    let [b, g, r, _] = pixel.to_le_bytes();
    [r, g, b].iter().zip(rgb).all(|(got, want)| got.abs_diff(want) <= 6)
}

#[test]
fn picture_and_input_cross_the_connection() {
    let fake = Arc::new(Fake::default());
    // More frames than may be in flight, so the viewer's acknowledgements are needed.
    let mut frames = vec![solid(200, 100, [0, 0, 255, 255]); 1];
    for step in 0..6u8 {
        let mut next = solid(200, 100, [0, 0, 255, 255]);
        for pixel in next.data[..4 * 64].as_chunks_mut::<4>().0 {
            *pixel = [step * 40, 200, 0, 255];
        }
        frames.push(next);
    }
    frames.push(solid(200, 100, [255, 0, 0, 255]));
    *fake.frames.lock().unwrap() = frames;
    let address = start(fake.clone(), PeerPolicy::Tailscale);

    let (events, received) = mpsc::channel();
    let client = Client::connect(
        address,
        &AccessKey::from_code(CODE).unwrap(),
        Options::default(),
        move |event| {
            let _ = events.send(event);
        },
    )
    .unwrap();
    wait_for("the last picture", || {
        let picture = client.picture().lock().unwrap();
        picture.width == 200
            && picture.height == 100
            && picture.pixels.iter().all(|pixel| close_to(*pixel, [0, 0, 255]))
    });
    assert_eq!(
        *fake.monitors.lock().unwrap(),
        [0],
        "the host's default monitor was used"
    );

    for message in [
        ClientMsg::MouseMove { x: 65535, y: 0 },
        ClientMsg::MouseButton {
            button: Button::Left,
            down: true,
        },
        ClientMsg::Key {
            scancode: 0x1E,
            down: true,
        },
        ClientMsg::Key {
            scancode: 0x1E,
            down: false,
        },
        ClientMsg::Key {
            scancode: 0xE05B,
            down: true,
        },
        ClientMsg::Wheel { dx: 0, dy: -120 },
    ] {
        client.send(message).unwrap();
    }
    wait_for("the input", || fake.input.lock().unwrap().len() == 6);
    client.close();
    // The key and the button still held are released when the viewer leaves.
    wait_for("the release", || fake.input.lock().unwrap().len() == 8);
    assert_eq!(
        *fake.input.lock().unwrap(),
        [
            "move 1199,0",
            "button Left true",
            "key 0x1e true",
            "key 0x1e false",
            "key 0xe05b true",
            "wheel 0,-120",
            "key 0xe05b false",
            "button Left false",
        ]
    );
    wait_for("the closed event", || {
        received.try_iter().any(|event| matches!(event, Event::Closed(_)))
    });
}

#[test]
fn a_wrong_key_is_refused_and_slows_the_next_attempt() {
    let fake = Arc::new(Fake::default());
    let address = start(fake.clone(), PeerPolicy::Tailscale);
    let wrong = AccessKey::from_code("ZZZZZ-ZZZZZ-ZZZZZ-ZZZZZ").unwrap();
    let error = Client::connect(address, &wrong, Options::default(), |_event| {}).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    // Inside the delay even the right key gets no answer.
    let right = AccessKey::from_code(CODE).unwrap();
    assert!(Client::connect(address, &right, Options::default(), |_event| {}).is_err());
    thread::sleep(Duration::from_millis(1100));
    let client = Client::connect(
        address,
        &right,
        Options {
            monitor: 1,
            ..Options::default()
        },
        |_event| {},
    )
    .unwrap();
    wait_for("the session", || *fake.monitors.lock().unwrap() == [1]);
    client.close();
}

#[test]
fn a_new_viewer_replaces_the_current_one() {
    let fake = Arc::new(Fake::default());
    *fake.frames.lock().unwrap() = vec![solid(64, 64, [9, 9, 9, 255])];
    let address = start(fake.clone(), PeerPolicy::Tailscale);
    let key = AccessKey::from_code(CODE).unwrap();
    let (events, received) = mpsc::channel();
    let _first = Client::connect(address, &key, Options::default(), move |event| {
        let _ = events.send(event);
    })
    .unwrap();
    wait_for("the first session", || fake.monitors.lock().unwrap().len() == 1);
    let second = Client::connect(address, &key, Options::default(), |_event| {}).unwrap();
    wait_for("the first viewer to be closed", || {
        received.try_iter().any(|event| matches!(event, Event::Closed(_)))
    });
    second.close();
}
