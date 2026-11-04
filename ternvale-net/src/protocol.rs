//! Length-prefixed messages. Versioned hello then DATA/LINK/DROP.

use std::io::{self, Read, Write};

/// Wire magic `TVN1`.
pub const MAGIC: &[u8; 4] = b"TVN1";
pub const MSG_DATA: u8 = 0;
pub const MSG_LINK: u8 = 1;
pub const MSG_DROP: u8 = 2;
const HDR: usize = 8;
const MAX: usize = 65_535;

/// Parameters advertised after the helper starts vmnet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    pub mtu: u32,
    pub max_packet: u32,
    pub max_batch: u32,
    pub mac: [u8; 6],
}

/// One framed message after hello.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Data(Vec<u8>),
    Link(bool),
    Drop(String),
}

impl Hello {
    #[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        let mut buf = [0u8; 24];
        buf[..4].copy_from_slice(MAGIC);
        buf[4..8].copy_from_slice(&self.mtu.to_le_bytes());
        buf[8..12].copy_from_slice(&self.max_packet.to_le_bytes());
        buf[12..18].copy_from_slice(&self.mac);
        buf[18..22].copy_from_slice(&self.max_batch.to_le_bytes());
        w.write_all(&buf)
    }

    #[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
    pub fn read_from(r: &mut impl Read) -> io::Result<Self> {
        let mut buf = [0u8; 24];
        r.read_exact(&mut buf)?;
        if &buf[..4] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad vmnet hello magic",
            ));
        }
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&buf[12..18]);
        Ok(Self {
            mtu: le_u32(&buf[4..8]),
            max_packet: le_u32(&buf[8..12]),
            mac,
            max_batch: le_u32(&buf[18..22]),
        })
    }
}

impl Msg {
    #[tracing::instrument(level = "trace", target = "ternvale::net::vmnet", skip_all)]
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        let (kind, payload): (u8, &[u8]) = match self {
            Self::Data(frame) => (MSG_DATA, frame),
            Self::Link(up) => (MSG_LINK, if *up { &[1] } else { &[0] }),
            Self::Drop(reason) => (MSG_DROP, reason.as_bytes()),
        };
        if payload.len() > MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "vmnet frame too large",
            ));
        }
        let mut hdr = [0u8; HDR];
        hdr[0] = kind;
        hdr[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        w.write_all(&hdr)?;
        w.write_all(payload)
    }

    #[tracing::instrument(level = "trace", target = "ternvale::net::vmnet", skip_all)]
    pub fn read_from(r: &mut impl Read) -> io::Result<Self> {
        let mut hdr = [0u8; HDR];
        r.read_exact(&mut hdr)?;
        let len = le_u32(&hdr[4..8]) as usize;
        if len > MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "vmnet length exceeds max",
            ));
        }
        let mut payload = vec![0u8; len];
        r.read_exact(&mut payload)?;
        match hdr[0] {
            MSG_DATA => Ok(Self::Data(payload)),
            MSG_LINK => Ok(Self::Link(payload.first().copied().unwrap_or(0) != 0)),
            MSG_DROP => Ok(Self::Drop(String::from_utf8_lossy(&payload).into_owned())),
            other => {
                tracing::warn!(
                    target: "ternvale::net::vmnet",
                    kind = other,
                    "unknown vmnet helper message"
                );
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unknown vmnet message",
                ))
            }
        }
    }
}

/// Pull one message from a byte buffer (non-blocking client).
pub fn take_msg(buf: &mut Vec<u8>) -> Option<io::Result<Msg>> {
    if buf.len() < HDR {
        return None;
    }
    let len = le_u32(&buf[4..8]) as usize;
    if len > MAX {
        buf.clear();
        return Some(Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "vmnet length exceeds max",
        )));
    }
    if buf.len() < HDR + len {
        return None;
    }
    let mut hdr = [0u8; HDR];
    hdr.copy_from_slice(&buf[..HDR]);
    let payload = buf[HDR..HDR + len].to_vec();
    buf.drain(..HDR + len);
    let msg = match hdr[0] {
        MSG_DATA => Msg::Data(payload),
        MSG_LINK => Msg::Link(payload.first().copied().unwrap_or(0) != 0),
        MSG_DROP => Msg::Drop(String::from_utf8_lossy(&payload).into_owned()),
        other => {
            return Some(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown vmnet message {other}"),
            )));
        }
    };
    Some(Ok(msg))
}

fn le_u32(bytes: &[u8]) -> u32 {
    let mut out = [0u8; 4];
    let n = bytes.len().min(4);
    out[..n].copy_from_slice(&bytes[..n]);
    u32::from_le_bytes(out)
}

#[cfg(test)]
mod tests {
    use super::{Hello, Msg};

    #[test]
    fn hello_and_msgs_round_trip() {
        let hello = Hello {
            mtu: 1500,
            max_packet: 1514,
            max_batch: 8,
            mac: [0x52, 0x54, 0, 1, 2, 3],
        };
        let mut buf = Vec::new();
        hello.write_to(&mut buf).expect("hello");
        let got = Hello::read_from(&mut buf.as_slice()).expect("read");
        assert_eq!(got, hello);
        buf.clear();
        Msg::Data(vec![0xaa, 0xbb])
            .write_to(&mut buf)
            .expect("data");
        let msg = Msg::read_from(&mut buf.as_slice()).expect("msg");
        assert_eq!(msg, Msg::Data(vec![0xaa, 0xbb]));
    }
}
