//! Wire format. After the handshake ([`crate::auth`]) each side sends
//! messages framed as `u32 length, u8 type, payload`, all little-endian.

use std::io::{self, Read, Write};

/// First bytes a viewer sends; the last byte is the protocol version.
pub const MAGIC: [u8; 8] = *b"OINKRD\x00\x01";
pub const DEFAULT_PORT: u16 = 47489;
/// Largest message a host may send (a full 8K frame fits).
pub const MAX_HOST_MESSAGE: usize = 96 << 20;
/// Largest message a viewer may send. Viewer messages are a few bytes each.
pub const MAX_CLIENT_MESSAGE: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl Button {
    fn to_wire(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Middle => 2,
            Self::Back => 3,
            Self::Forward => 4,
        }
    }
    fn from_wire(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Left,
            1 => Self::Right,
            2 => Self::Middle,
            3 => Self::Back,
            4 => Self::Forward,
            _ => return None,
        })
    }
}

/// Viewer to host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientMsg {
    /// First message. `quality` is the JPEG quality while the picture moves.
    Hello {
        monitor: u8,
        max_fps: u8,
        quality: u8,
    },
    /// The frame with this sequence number has been shown.
    Ack {
        seq: u32,
    },
    /// Pointer position, each axis scaled to 0..=65535 across the monitor.
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: Button,
        down: bool,
    },
    /// Wheel movement in Windows units (120 per notch). Positive `dy` scrolls up,
    /// positive `dx` scrolls right.
    Wheel {
        dx: i16,
        dy: i16,
    },
    /// PC scan code (set 1); extended keys have [`crate::keymap::EXTENDED`] set.
    Key {
        scancode: u16,
        down: bool,
    },
    /// Release every key and button the viewer holds down (focus left the window).
    ReleaseAll,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// A baseline JPEG of exactly `w` by `h` pixels.
    pub data: Vec<u8>,
}

/// Host to viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostMsg {
    /// Size of the picture. Sent first and whenever the monitor's mode changes;
    /// the viewer starts from a black picture of this size.
    Screen {
        width: u32,
        height: u32,
        monitor: u8,
        monitor_count: u8,
    },
    Frame {
        seq: u32,
        rects: Vec<EncodedRect>,
    },
    /// Nothing changed; the connection is alive.
    Ping,
}

fn invalid(text: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, text)
}

fn write_framed(out: &mut impl Write, body: &[u8]) -> io::Result<()> {
    out.write_all(&(body.len() as u32).to_le_bytes())?;
    out.write_all(body)
}

fn read_framed(input: &mut impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut length = [0u8; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > limit {
        return Err(invalid("message length out of range"));
    }
    let mut body = vec![0u8; length];
    input.read_exact(&mut body)?;
    Ok(body)
}

struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn take(&mut self, count: usize) -> io::Result<&'a [u8]> {
        if self.0.len() < count {
            return Err(invalid("message too short"));
        }
        let (head, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(head)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn end(self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(invalid("message too long"))
        }
    }
}

impl ClientMsg {
    pub fn write(&self, out: &mut impl Write) -> io::Result<()> {
        let mut body = Vec::with_capacity(8);
        match *self {
            Self::Hello {
                monitor,
                max_fps,
                quality,
            } => body.extend([1, monitor, max_fps, quality]),
            Self::Ack { seq } => {
                body.push(2);
                body.extend(seq.to_le_bytes());
            }
            Self::MouseMove { x, y } => {
                body.push(3);
                body.extend(x.to_le_bytes());
                body.extend(y.to_le_bytes());
            }
            Self::MouseButton { button, down } => body.extend([4, button.to_wire(), down as u8]),
            Self::Wheel { dx, dy } => {
                body.push(5);
                body.extend(dx.to_le_bytes());
                body.extend(dy.to_le_bytes());
            }
            Self::Key { scancode, down } => {
                body.push(6);
                body.extend(scancode.to_le_bytes());
                body.push(down as u8);
            }
            Self::ReleaseAll => body.push(7),
        }
        write_framed(out, &body)
    }

    pub fn read(input: &mut impl Read) -> io::Result<Self> {
        let body = read_framed(input, MAX_CLIENT_MESSAGE)?;
        let mut fields = Fields(&body);
        let message = match fields.u8()? {
            1 => Self::Hello {
                monitor: fields.u8()?,
                max_fps: fields.u8()?,
                quality: fields.u8()?,
            },
            2 => Self::Ack { seq: fields.u32()? },
            3 => Self::MouseMove {
                x: fields.u16()?,
                y: fields.u16()?,
            },
            4 => Self::MouseButton {
                button: Button::from_wire(fields.u8()?).ok_or_else(|| invalid("unknown button"))?,
                down: fields.u8()? != 0,
            },
            5 => Self::Wheel {
                dx: fields.u16()? as i16,
                dy: fields.u16()? as i16,
            },
            6 => Self::Key {
                scancode: fields.u16()?,
                down: fields.u8()? != 0,
            },
            7 => Self::ReleaseAll,
            _ => return Err(invalid("unknown viewer message")),
        };
        fields.end()?;
        Ok(message)
    }
}

impl HostMsg {
    pub fn write(&self, out: &mut impl Write) -> io::Result<()> {
        match self {
            Self::Screen {
                width,
                height,
                monitor,
                monitor_count,
            } => {
                let mut body = vec![1];
                body.extend(width.to_le_bytes());
                body.extend(height.to_le_bytes());
                body.extend([*monitor, *monitor_count]);
                write_framed(out, &body)
            }
            Self::Frame { seq, rects } => {
                let size: usize = 7 + rects.iter().map(|rect| 12 + rect.data.len()).sum::<usize>();
                if size > MAX_HOST_MESSAGE || rects.len() > u16::MAX as usize {
                    return Err(invalid("frame too large"));
                }
                let mut body = Vec::with_capacity(size);
                body.push(2);
                body.extend(seq.to_le_bytes());
                body.extend((rects.len() as u16).to_le_bytes());
                for rect in rects {
                    for value in [rect.x, rect.y, rect.w, rect.h] {
                        body.extend(value.to_le_bytes());
                    }
                    body.extend((rect.data.len() as u32).to_le_bytes());
                    body.extend(&rect.data);
                }
                write_framed(out, &body)
            }
            Self::Ping => write_framed(out, &[3]),
        }
    }

    pub fn read(input: &mut impl Read) -> io::Result<Self> {
        let body = read_framed(input, MAX_HOST_MESSAGE)?;
        let mut fields = Fields(&body);
        let message = match fields.u8()? {
            1 => Self::Screen {
                width: fields.u32()?,
                height: fields.u32()?,
                monitor: fields.u8()?,
                monitor_count: fields.u8()?,
            },
            2 => {
                let seq = fields.u32()?;
                let count = fields.u16()? as usize;
                let mut rects = Vec::with_capacity(count.min(4096));
                for _ in 0..count {
                    let (x, y, w, h) = (fields.u16()?, fields.u16()?, fields.u16()?, fields.u16()?);
                    let length = fields.u32()? as usize;
                    rects.push(EncodedRect {
                        x,
                        y,
                        w,
                        h,
                        data: fields.take(length)?.to_vec(),
                    });
                }
                Self::Frame { seq, rects }
            }
            3 => Self::Ping,
            _ => return Err(invalid("unknown host message")),
        };
        fields.end()?;
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_messages_round_trip() {
        let messages = [
            ClientMsg::Hello {
                monitor: 1,
                max_fps: 30,
                quality: 60,
            },
            ClientMsg::Ack { seq: 0xDEAD_BEEF },
            ClientMsg::MouseMove { x: 65535, y: 1 },
            ClientMsg::MouseButton {
                button: Button::Forward,
                down: true,
            },
            ClientMsg::Wheel { dx: -120, dy: 37 },
            ClientMsg::Key {
                scancode: 0xE01D,
                down: false,
            },
            ClientMsg::ReleaseAll,
        ];
        let mut bytes = Vec::new();
        for message in &messages {
            message.write(&mut bytes).unwrap();
        }
        let mut input = bytes.as_slice();
        for message in &messages {
            assert_eq!(&ClientMsg::read(&mut input).unwrap(), message);
        }
        assert!(input.is_empty());
    }

    #[test]
    fn host_messages_round_trip() {
        let messages = [
            HostMsg::Screen {
                width: 2560,
                height: 1440,
                monitor: 0,
                monitor_count: 2,
            },
            HostMsg::Frame {
                seq: 7,
                rects: vec![
                    EncodedRect {
                        x: 0,
                        y: 64,
                        w: 128,
                        h: 64,
                        data: vec![1, 2, 3],
                    },
                    EncodedRect {
                        x: 5,
                        y: 6,
                        w: 7,
                        h: 8,
                        data: vec![],
                    },
                ],
            },
            HostMsg::Ping,
        ];
        let mut bytes = Vec::new();
        for message in &messages {
            message.write(&mut bytes).unwrap();
        }
        let mut input = bytes.as_slice();
        for message in &messages {
            assert_eq!(&HostMsg::read(&mut input).unwrap(), message);
        }
    }

    #[test]
    fn malformed_messages_are_rejected() {
        // Oversized viewer message, unknown type, truncated and overlong payloads.
        assert!(ClientMsg::read(&mut [200u8, 0, 0, 0].as_slice()).is_err());
        assert!(ClientMsg::read(&mut [1u8, 0, 0, 0, 99].as_slice()).is_err());
        assert!(ClientMsg::read(&mut [2u8, 0, 0, 0, 2, 1].as_slice()).is_err());
        assert!(ClientMsg::read(&mut [2u8, 0, 0, 0, 7, 1].as_slice()).is_err());
        assert!(ClientMsg::read(&mut [3u8, 0, 0, 0, 4, 9, 1].as_slice()).is_err());
        // A rectangle that claims more bytes than the message holds.
        let mut frame = vec![2u8, 0, 0, 0, 0, 1, 0];
        frame.extend([0u8; 8]);
        frame.extend(1000u32.to_le_bytes());
        let mut bytes = (frame.len() as u32).to_le_bytes().to_vec();
        bytes.extend(frame);
        assert!(HostMsg::read(&mut bytes.as_slice()).is_err());
    }
}
