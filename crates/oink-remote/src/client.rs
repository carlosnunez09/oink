//! Viewer side of the connection: keeps the picture up to date on its own
//! thread and sends input. The window ([`crate::viewer`]) is separate.

use crate::{
    auth::{self, AccessKey},
    codec::{self, Picture},
    proto::{ClientMsg, HostMsg},
};
use std::{
    io,
    net::{Shutdown, TcpStream, ToSocketAddrs},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The host sends something at least every 2 s.
const SILENCE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Monitor number on the host, or [`crate::host::DEFAULT_MONITOR`].
    pub monitor: u8,
    pub max_fps: u8,
    /// JPEG quality while the picture moves, 10 to 95.
    pub quality: u8,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            monitor: crate::host::DEFAULT_MONITOR,
            max_fps: 30,
            quality: 60,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The picture changed (also its size).
    Updated,
    /// The connection ended; nothing more follows.
    Closed(String),
}

#[derive(Debug)]
pub struct Client {
    writer: Mutex<TcpStream>,
    picture: Arc<Mutex<Picture>>,
}

impl Client {
    /// Connect, prove the key and start receiving. `notify` runs on the receiving thread.
    pub fn connect(
        address: impl ToSocketAddrs,
        key: &AccessKey,
        options: Options,
        notify: impl Fn(Event) + Send + 'static,
    ) -> io::Result<Arc<Self>> {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "the name has no address");
        let mut stream = None;
        for address in address.to_socket_addrs()? {
            match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
                Ok(connected) => {
                    stream = Some(connected);
                    break;
                }
                Err(error) => last = error,
            }
        }
        let Some(mut stream) = stream else { return Err(last) };
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
        stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;
        auth::viewer_handshake(&mut stream, key)?;
        ClientMsg::Hello {
            monitor: options.monitor,
            max_fps: options.max_fps,
            quality: options.quality,
        }
        .write(&mut stream)?;
        stream.set_read_timeout(Some(SILENCE_TIMEOUT))?;
        let client = Arc::new(Self {
            writer: Mutex::new(stream.try_clone()?),
            picture: Arc::default(),
        });
        let receiver = client.clone();
        thread::Builder::new()
            .name("oink-remote-receive".to_owned())
            .spawn(move || {
                let reason = match receiver.receive(stream, &notify) {
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                        "the computer closed the connection".into()
                    }
                    Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => {
                        "no answer from the computer".into()
                    }
                    Err(error) => error.to_string(),
                    Ok(()) => String::new(),
                };
                notify(Event::Closed(reason));
            })?;
        Ok(client)
    }

    fn receive(&self, mut stream: TcpStream, notify: &dyn Fn(Event)) -> io::Result<()> {
        loop {
            match HostMsg::read(&mut stream)? {
                HostMsg::Screen { width, height, .. } => {
                    if width == 0 || height == 0 || width > 32768 || height > 32768 {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "impossible screen size"));
                    }
                    *self.picture.lock().unwrap() = Picture::new(width as usize, height as usize);
                    notify(Event::Updated);
                }
                HostMsg::Frame { seq, rects } => {
                    let decoded = codec::decode_all(&rects)?;
                    {
                        let mut picture = self.picture.lock().unwrap();
                        for rect in &decoded {
                            picture.draw(rect)?;
                        }
                    }
                    self.send(ClientMsg::Ack { seq })?;
                    notify(Event::Updated);
                }
                HostMsg::Ping => {}
            }
        }
    }

    pub fn send(&self, message: ClientMsg) -> io::Result<()> {
        message.write(&mut *self.writer.lock().unwrap())
    }

    /// The current picture. Hold the lock only while drawing.
    pub fn picture(&self) -> &Mutex<Picture> {
        &self.picture
    }

    pub fn close(&self) {
        let _ = self.writer.lock().unwrap().shutdown(Shutdown::Both);
    }
}
