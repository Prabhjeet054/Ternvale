//! Length-prefixed messages. Versioned hello then DATA/LINK/DROP.

use std::io::{self, Read, Write};

/// Wire magic `TVN1` (helper hello).
pub const MAGIC: &[u8; 4] = b"TVN1";
/// Client open request.
pub const OPEN_MAGIC: &[u8; 4] = b"TVNO";
/// Helper start failure (UTF-8 message).
pub const ERR_MAGIC: &[u8; 4] = b"TVNE";
pub const MSG_DATA: u8 = 0;
pub const MSG_LINK: u8 = 1;
pub const MSG_DROP: u8 = 2;
const HDR: usize = 8;
const MAX: usize = 65_535;
const OPEN_NET: usize = 64;
const OPEN_IF: usize = 16;
const OPEN_LEN: usize = 4 + 4 + OPEN_NET + OPEN_IF;

/// Client request: vmnet mode plus optional host-only name / bridge NIC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    /// `VMNET_*_MODE` (1000/1001/1002).
    pub mode: u32,
    /// Host-only LAN name.
    pub network: String,
    /// Bridged physical interface.
    pub bridge: String,
}

impl Default for Open {
    fn default() -> Self {
        Self {
            mode: 1001,
            network: String::new(),
            bridge: String::new(),
        }
    }
}

impl Open {
    #[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        let mut buf = [0u8; OPEN_LEN];
        buf[..4].copy_from_slice(OPEN_MAGIC);
        buf[4..8].copy_from_slice(&self.mode.to_le_bytes());
        put_str(&mut buf[8..8 + OPEN_NET], &self.network);
        put_str(&mut buf[8 + OPEN_NET..], &self.bridge);
        w.write_all(&buf)
    }

    #[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
    pub fn read_from(r: &mut impl Read) -> io::Result<Self> {
        let mut buf = [0u8; OPEN_LEN];
        r.read_exact(&mut buf)?;
        if &buf[..4] != OPEN_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad vmnet open magic",
            ));
        }
        Ok(Self {
            mode: le_u32(&buf[4..8]),
            network: take_str(&buf[8..8 + OPEN_NET]),
            bridge: take_str(&buf[8 + OPEN_NET..]),
        })
    }
}

/// Write `TVNE` + length + message so the client can show a real reason.
#[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
pub fn write_error(w: &mut impl Write, message: &str) -> io::Result<()> {
    let bytes = message.as_bytes();
    let len = bytes.len().min(2048) as u32;
    w.write_all(ERR_MAGIC)?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&bytes[..len as usize])
}

fn put_str(dst: &mut [u8], s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(dst.len());
    dst[..n].copy_from_slice(&bytes[..n]);
}

fn take_str(src: &[u8]) -> String {
    let end = src.iter().position(|b| *b == 0).unwrap_or(src.len());
    String::from_utf8_lossy(&src[..end]).into_owned()
}

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
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if magic == *ERR_MAGIC {
            let mut lenb = [0u8; 4];
            r.read_exact(&mut lenb)?;
            let len = u32::from_le_bytes(lenb) as usize;
            if len > 2048 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "vmnet error too long",
                ));
            }
            let mut msg = vec![0u8; len];
            r.read_exact(&mut msg)?;
            return Err(io::Error::other(String::from_utf8_lossy(&msg).into_owned()));
        }
        if magic != *MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad vmnet hello magic",
            ));
        }
        let mut rest = [0u8; 20];
        r.read_exact(&mut rest)?;
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&rest[8..14]);
        Ok(Self {
            mtu: le_u32(&rest[0..4]),
            max_packet: le_u32(&rest[4..8]),
            mac,
            max_batch: le_u32(&rest[14..18]),
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
    use super::{write_error, Hello, Msg, Open};

    #[test]
    fn open_round_trip() {
        let open = Open {
            mode: 1000,
            network: "lab".into(),
            bridge: String::new(),
        };
        let mut buf = Vec::new();
        open.write_to(&mut buf).expect("open");
        let got = Open::read_from(&mut buf.as_slice()).expect("read");
        assert_eq!(got, open);
    }

    #[test]
    fn hello_error_is_readable() {
        let mut buf = Vec::new();
        write_error(&mut buf, "bridged needs com.apple.vm.networking").expect("err");
        let err = Hello::read_from(&mut buf.as_slice()).expect_err("tvne");
        assert!(err.to_string().contains("com.apple.vm.networking"), "{err}");
    }

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
