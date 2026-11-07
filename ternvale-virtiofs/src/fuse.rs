//! FUSE on-wire headers (Linux `fuse.h`, little-endian). virtio-fs carries
//! one request per virtqueue chain: `fuse_in_header` plus args in the
//! readable buffers, `fuse_out_header` plus results in the writable ones.

/// `FUSE_ROOT_ID`.
pub const ROOT_ID: u64 = 1;
/// Size of `fuse_in_header`.
pub const IN_HEADER: usize = 40;
/// Size of `fuse_out_header`.
pub const OUT_HEADER: usize = 16;
/// Size of `fuse_attr`.
pub const ATTR_SIZE: usize = 88;
/// Size of `fuse_entry_out` without padding beyond attr.
pub const ENTRY_OUT: usize = 40 + ATTR_SIZE;
/// FUSE kernel ABI major we speak.
pub const KERNEL_MAJOR: u32 = 7;
/// FUSE kernel ABI minor. TODO(verify): virtio-fs Linux guests; 31 matches
/// `FUSE_INIT` fields through `time_gran`.
pub const KERNEL_MINOR: u32 = 31;

/// Parsed request header.
#[derive(Debug, Clone, Copy)]
pub struct InHeader {
    /// Total request length.
    pub len: u32,
    /// Opcode.
    pub opcode: u32,
    /// Unique id; echoed in the reply (except FORGET).
    pub unique: u64,
    /// Inode the request is about.
    pub nodeid: u64,
}

/// `fuse_attr` fields we fill from `stat`.
#[derive(Debug, Clone, Copy)]
pub struct Attr {
    /// FUSE node id (not the host inode).
    pub ino: u64,
    pub size: u64,
    pub blocks: u64,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub atimensec: u32,
    pub mtimensec: u32,
    pub ctimensec: u32,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u32,
    pub blksize: u32,
}

impl InHeader {
    /// Parse the 40-byte in-header. Guest-controlled; rejects short buffers.
    #[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all, fields(len = buf.len()))]
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < IN_HEADER {
            return None;
        }
        let len = u32::from_le_bytes(buf[0..4].try_into().ok()?);
        Some(Self {
            len,
            opcode: u32::from_le_bytes(buf[4..8].try_into().ok()?),
            unique: u64::from_le_bytes(buf[8..16].try_into().ok()?),
            nodeid: u64::from_le_bytes(buf[16..24].try_into().ok()?),
        })
    }
}

/// NUL-terminated name from `bytes`. Empty, overlong, `.`, `..`, and `/` are
/// rejected by [`crate::name::check`].
#[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all)]
pub fn c_string(bytes: &[u8]) -> Option<&str> {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).ok()
}

/// Out header plus payload. `error` is a positive errno; stored negated.
#[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all, fields(unique, error, payload = payload.len()))]
pub fn reply(unique: u64, error: i32, payload: &[u8]) -> Vec<u8> {
    let len = (OUT_HEADER + payload.len()) as u32;
    let mut out = Vec::with_capacity(len as usize);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(-error).to_le_bytes());
    out.extend_from_slice(&unique.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Encode `fuse_attr`.
#[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all)]
pub fn encode_attr(attr: &Attr) -> [u8; ATTR_SIZE] {
    let mut buf = [0u8; ATTR_SIZE];
    write_u64(&mut buf, 0, attr.ino);
    write_u64(&mut buf, 8, attr.size);
    write_u64(&mut buf, 16, attr.blocks);
    write_u64(&mut buf, 24, attr.atime);
    write_u64(&mut buf, 32, attr.mtime);
    write_u64(&mut buf, 40, attr.ctime);
    write_u32(&mut buf, 48, attr.atimensec);
    write_u32(&mut buf, 52, attr.mtimensec);
    write_u32(&mut buf, 56, attr.ctimensec);
    write_u32(&mut buf, 60, attr.mode);
    write_u32(&mut buf, 64, attr.nlink);
    write_u32(&mut buf, 68, attr.uid);
    write_u32(&mut buf, 72, attr.gid);
    write_u32(&mut buf, 76, attr.rdev);
    write_u32(&mut buf, 80, attr.blksize);
    buf
}

/// `fuse_entry_out`.
#[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all)]
pub fn encode_entry(nodeid: u64, generation: u64, attr: &Attr) -> Vec<u8> {
    let mut out = Vec::with_capacity(ENTRY_OUT);
    out.extend_from_slice(&nodeid.to_le_bytes());
    out.extend_from_slice(&generation.to_le_bytes());
    // entry_valid / attr_valid: 1 second.
    out.extend_from_slice(&1u64.to_le_bytes());
    out.extend_from_slice(&1u64.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&encode_attr(attr));
    out
}

fn write_u64(buf: &mut [u8], at: usize, value: u64) {
    buf[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Pad a dirent record to 8 bytes.
#[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all)]
pub fn dirent_record(ino: u64, off: u64, typ: u32, name: &str) -> Vec<u8> {
    let namelen = name.len() as u32;
    let reclen = (24 + name.len() + 7) & !7;
    let mut rec = vec![0u8; reclen];
    rec[0..8].copy_from_slice(&ino.to_le_bytes());
    rec[8..16].copy_from_slice(&off.to_le_bytes());
    rec[16..20].copy_from_slice(&namelen.to_le_bytes());
    rec[20..24].copy_from_slice(&typ.to_le_bytes());
    rec[24..24 + name.len()].copy_from_slice(name.as_bytes());
    rec
}

#[cfg(test)]
mod tests {
    use super::{encode_attr, reply, Attr, InHeader, IN_HEADER};

    #[test]
    fn parses_in_header() {
        let mut buf = vec![0u8; IN_HEADER];
        buf[0..4].copy_from_slice(&40u32.to_le_bytes());
        buf[4..8].copy_from_slice(&26u32.to_le_bytes());
        buf[8..16].copy_from_slice(&7u64.to_le_bytes());
        buf[16..24].copy_from_slice(&1u64.to_le_bytes());
        let hdr = InHeader::parse(&buf).expect("hdr");
        assert_eq!(hdr.opcode, 26);
        assert_eq!(hdr.unique, 7);
        assert_eq!(hdr.nodeid, 1);
    }

    #[test]
    fn reply_negates_errno() {
        let out = reply(3, libc::ENOENT, &[]);
        assert_eq!(&out[0..4], &16u32.to_le_bytes());
        assert_eq!(&out[4..8], &(-libc::ENOENT).to_le_bytes());
        assert_eq!(&out[8..16], &3u64.to_le_bytes());
    }

    #[test]
    fn attr_round_trips_ino() {
        let attr = Attr {
            ino: 9,
            size: 0,
            blocks: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
            atimensec: 0,
            mtimensec: 0,
            ctimensec: 0,
            mode: 0o644,
            nlink: 1,
            uid: 1,
            gid: 1,
            rdev: 0,
            blksize: 4096,
        };
        let bytes = encode_attr(&attr);
        assert_eq!(&bytes[0..8], &9u64.to_le_bytes());
    }
}
