//! `struct virtio_vsock_hdr` (virtio 1.2, 5.10.6): 44 little-endian bytes.

/// Header size in bytes.
pub const HDR_LEN: usize = 44;
/// Largest payload in one packet (`VIRTIO_VSOCK_MAX_PKT_BUF_SIZE` in Linux).
pub const MAX_PAYLOAD: usize = 64 * 1024;

/// `VIRTIO_VSOCK_TYPE_STREAM`.
pub const TYPE_STREAM: u16 = 1;

/// `VIRTIO_VSOCK_OP_REQUEST`: open a connection.
pub const OP_REQUEST: u16 = 1;
/// `VIRTIO_VSOCK_OP_RESPONSE`: accept a connection.
pub const OP_RESPONSE: u16 = 2;
/// `VIRTIO_VSOCK_OP_RST`: refuse or abort a connection.
pub const OP_RST: u16 = 3;
/// `VIRTIO_VSOCK_OP_SHUTDOWN`: half or full close; see the `SHUTDOWN_*` flags.
pub const OP_SHUTDOWN: u16 = 4;
/// `VIRTIO_VSOCK_OP_RW`: stream data.
pub const OP_RW: u16 = 5;
/// `VIRTIO_VSOCK_OP_CREDIT_UPDATE`: new `buf_alloc` / `fwd_cnt`.
pub const OP_CREDIT_UPDATE: u16 = 6;
/// `VIRTIO_VSOCK_OP_CREDIT_REQUEST`: ask the peer for a credit update.
pub const OP_CREDIT_REQUEST: u16 = 7;

/// `VIRTIO_VSOCK_SHUTDOWN_RCV`: the sender will receive no more data.
pub const SHUTDOWN_RCV: u32 = 1;
/// `VIRTIO_VSOCK_SHUTDOWN_SEND`: the sender will send no more data.
pub const SHUTDOWN_SEND: u32 = 2;
/// Both shutdown flags.
pub const SHUTDOWN_BOTH: u32 = SHUTDOWN_RCV | SHUTDOWN_SEND;

/// One decoded packet header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Header {
    /// Source context id.
    pub src_cid: u64,
    /// Destination context id.
    pub dst_cid: u64,
    /// Source port.
    pub src_port: u32,
    /// Destination port.
    pub dst_port: u32,
    /// Payload length after the header.
    pub len: u32,
    /// Socket type (`TYPE_STREAM`).
    pub kind: u16,
    /// Operation (`OP_*`).
    pub op: u16,
    /// Operation flags (`SHUTDOWN_*` for `OP_SHUTDOWN`).
    pub flags: u32,
    /// Sender's receive buffer size for this connection.
    pub buf_alloc: u32,
    /// Bytes the sender has consumed from its receive buffer, modulo 2^32.
    pub fwd_cnt: u32,
}

impl Header {
    /// Decode the first [`HDR_LEN`] bytes. `None` when `bytes` is shorter.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let b = bytes.get(..HDR_LEN)?;
        let u64_at = |o: usize| {
            u64::from_le_bytes([
                b[o],
                b[o + 1],
                b[o + 2],
                b[o + 3],
                b[o + 4],
                b[o + 5],
                b[o + 6],
                b[o + 7],
            ])
        };
        let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        Some(Self {
            src_cid: u64_at(0),
            dst_cid: u64_at(8),
            src_port: u32_at(16),
            dst_port: u32_at(20),
            len: u32_at(24),
            kind: u16_at(28),
            op: u16_at(30),
            flags: u32_at(32),
            buf_alloc: u32_at(36),
            fwd_cnt: u32_at(40),
        })
    }

    /// Encode to the wire layout.
    pub fn encode(&self) -> [u8; HDR_LEN] {
        let mut b = [0u8; HDR_LEN];
        b[0..8].copy_from_slice(&self.src_cid.to_le_bytes());
        b[8..16].copy_from_slice(&self.dst_cid.to_le_bytes());
        b[16..20].copy_from_slice(&self.src_port.to_le_bytes());
        b[20..24].copy_from_slice(&self.dst_port.to_le_bytes());
        b[24..28].copy_from_slice(&self.len.to_le_bytes());
        b[28..30].copy_from_slice(&self.kind.to_le_bytes());
        b[30..32].copy_from_slice(&self.op.to_le_bytes());
        b[32..36].copy_from_slice(&self.flags.to_le_bytes());
        b[36..40].copy_from_slice(&self.buf_alloc.to_le_bytes());
        b[40..44].copy_from_slice(&self.fwd_cnt.to_le_bytes());
        b
    }
}

/// Spec name of `op`, for logs.
pub fn op_name(op: u16) -> &'static str {
    match op {
        OP_REQUEST => "REQUEST",
        OP_RESPONSE => "RESPONSE",
        OP_RST => "RST",
        OP_SHUTDOWN => "SHUTDOWN",
        OP_RW => "RW",
        OP_CREDIT_UPDATE => "CREDIT_UPDATE",
        OP_CREDIT_REQUEST => "CREDIT_REQUEST",
        _ => "INVALID",
    }
}

/// TRACE one header on `ternvale::virtio::vsock`. `direction` is `tx` (guest to
/// host) or `rx` (host to guest).
pub(super) fn trace(direction: &'static str, h: &Header) {
    tracing::trace!(
        target: "ternvale::virtio::vsock",
        direction,
        src_cid = h.src_cid,
        src_port = h.src_port,
        dst_cid = h.dst_cid,
        dst_port = h.dst_port,
        op = op_name(h.op),
        kind = h.kind,
        len = h.len,
        flags = %format!("{:#x}", h.flags),
        buf_alloc = h.buf_alloc,
        fwd_cnt = h.fwd_cnt,
        "vsock packet"
    );
}

#[cfg(test)]
mod tests {
    use super::{op_name, Header, HDR_LEN, OP_CREDIT_REQUEST, OP_RW, TYPE_STREAM};

    #[test]
    fn encodes_and_parses_the_wire_layout() {
        let h = Header {
            src_cid: 3,
            dst_cid: 2,
            src_port: 0x1234_5678,
            dst_port: 5000,
            len: 4,
            kind: TYPE_STREAM,
            op: OP_RW,
            flags: 0,
            buf_alloc: 0x0004_0000,
            fwd_cnt: 9,
        };
        let b = h.encode();
        assert_eq!(b.len(), HDR_LEN);
        assert_eq!(&b[0..8], &3u64.to_le_bytes());
        assert_eq!(&b[16..20], &[0x78, 0x56, 0x34, 0x12]);
        assert_eq!(&b[28..32], &[1, 0, 5, 0], "type then op");
        assert_eq!(Header::parse(&b), Some(h));
        assert_eq!(Header::parse(&b[..HDR_LEN - 1]), None, "short header");
    }

    #[test]
    fn trace_logs_every_header_field() {
        let dir = std::env::temp_dir().join(format!("tv-vsock-trace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("log dir");
        let mut config = ternvale_log::LogConfig::new("vsock-trace", dir.clone());
        config.level = "ternvale::virtio::vsock=trace".to_string();
        let guard = ternvale_log::init(config).expect("log");
        let h = Header {
            src_cid: 3,
            dst_cid: 2,
            src_port: 1025,
            dst_port: 5000,
            len: 4,
            kind: TYPE_STREAM,
            op: OP_RW,
            flags: 0,
            buf_alloc: 262_144,
            fwd_cnt: 7,
        };
        super::trace("tx", &h);
        let path = guard.log_path().to_path_buf();
        drop(guard);
        let text = std::fs::read_to_string(&path).expect("log");
        std::fs::remove_dir_all(&dir).expect("remove log dir");
        for field in [
            "vsock packet",
            "direction=\"tx\"",
            "src_cid=3",
            "src_port=1025",
            "dst_cid=2",
            "dst_port=5000",
            "op=\"RW\"",
            "len=4",
            "flags=0x0",
            "buf_alloc=262144",
            "fwd_cnt=7",
        ] {
            assert!(text.contains(field), "missing {field}: {text}");
        }
    }

    #[test]
    fn names_ops() {
        assert_eq!(op_name(OP_RW), "RW");
        assert_eq!(op_name(OP_CREDIT_REQUEST), "CREDIT_REQUEST");
        assert_eq!(op_name(0), "INVALID");
        assert_eq!(op_name(99), "INVALID");
    }
}
