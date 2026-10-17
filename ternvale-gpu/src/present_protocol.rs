//! Wire format between the VMM's presenter and the dev viewer
//! (`tools/ternvale-viewer`, Step 44) over a Unix stream socket.
//!
//! Every integer is little-endian. Both directions start with the magic
//! `"TVDP"` and a version, so a stale viewer binary is refused with a clear
//! error instead of misreading frames.
//!
//! - VMM → viewer: a [`FrameHeader`] ([`FRAME_HEADER_LEN`] bytes) followed by
//!   `payload_len` bytes of pixels (`height` rows of `stride` bytes, in the
//!   virtio-gpu `format`; `format` 0 with an empty payload means "no scanout").
//! - viewer → VMM: fixed [`MESSAGE_LEN`]-byte messages: `HELLO` first (carrying
//!   the viewer's version), then `PRESENTED` with a frame's `sequence` once it
//!   is on screen. Step 45 adds input messages here.

/// `"TVDP"` read as a little-endian u32.
pub const MAGIC: u32 = u32::from_le_bytes(*b"TVDP");
/// Wire version; a peer with another version is refused.
pub const VERSION: u16 = 1;
/// Bytes in a [`FrameHeader`].
pub const FRAME_HEADER_LEN: usize = 40;
/// Bytes in every viewer → VMM message.
pub const MESSAGE_LEN: usize = 16;
/// VMM → viewer: one frame.
pub const KIND_FRAME: u16 = 0x0001;
/// viewer → VMM: first message, `value` = the viewer's wire version.
pub const KIND_HELLO: u16 = 0x0101;
/// viewer → VMM: frame `value` (its sequence) is on screen.
pub const KIND_PRESENTED: u16 = 0x0102;
/// Largest width or height a frame may claim (the device's own limit).
pub const MAX_DIMENSION: u32 = crate::gpu2d::MAX_DIMENSION;

/// A malformed or incompatible message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// Fewer bytes than the fixed layout.
    #[error("{what} is {len} bytes, need {need}")]
    Short {
        /// Which message.
        what: &'static str,
        /// Bytes given.
        len: usize,
        /// Bytes required.
        need: usize,
    },
    /// Not `"TVDP"`.
    #[error("bad magic {found:#010x} (want {MAGIC:#010x} \"TVDP\"); not a ternvale viewer?")]
    BadMagic {
        /// What arrived.
        found: u32,
    },
    /// The peer speaks another version.
    #[error("wire version {found} is not supported (this build speaks {VERSION}); rebuild the viewer with scripts/build-viewer.sh")]
    Version {
        /// The peer's version.
        found: u16,
    },
    /// A message type this side does not know.
    #[error("unknown message kind {kind:#06x}")]
    Kind {
        /// The type field.
        kind: u16,
    },
    /// Geometry that does not describe its payload.
    #[error("frame {width}x{height} stride {stride} does not match a {payload_len}-byte payload")]
    Geometry {
        /// Claimed width.
        width: u32,
        /// Claimed height.
        height: u32,
        /// Claimed stride.
        stride: u32,
        /// Claimed payload length.
        payload_len: u32,
    },
}

/// Header of one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Pixels per row (0: no scanout).
    pub width: u32,
    /// Rows.
    pub height: u32,
    /// Bytes per row (`width × 4`).
    pub stride: u32,
    /// `VIRTIO_GPU_FORMAT_*`, 0 when there is no scanout.
    pub format: u32,
    /// The scanout's update counter ([`crate::Frame::sequence`]).
    pub sequence: u64,
    /// Pixel bytes after the header (`stride × height`).
    pub payload_len: u32,
}

impl FrameHeader {
    /// The 40-byte wire form.
    pub fn encode(&self) -> [u8; FRAME_HEADER_LEN] {
        let mut out = [0u8; FRAME_HEADER_LEN];
        out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        out[4..6].copy_from_slice(&VERSION.to_le_bytes());
        out[6..8].copy_from_slice(&KIND_FRAME.to_le_bytes());
        out[8..12].copy_from_slice(&self.width.to_le_bytes());
        out[12..16].copy_from_slice(&self.height.to_le_bytes());
        out[16..20].copy_from_slice(&self.stride.to_le_bytes());
        out[20..24].copy_from_slice(&self.format.to_le_bytes());
        out[24..32].copy_from_slice(&self.sequence.to_le_bytes());
        out[32..36].copy_from_slice(&self.payload_len.to_le_bytes());
        out
    }

    /// Parse and check a header the way the viewer does.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let kind = check_preamble("frame header", bytes, FRAME_HEADER_LEN)?;
        if kind != KIND_FRAME {
            return Err(WireError::Kind { kind });
        }
        let header = Self {
            width: le32(bytes, 8),
            height: le32(bytes, 12),
            stride: le32(bytes, 16),
            format: le32(bytes, 20),
            sequence: le64(bytes, 24),
            payload_len: le32(bytes, 32),
        };
        let fits = header.width <= MAX_DIMENSION
            && header.height <= MAX_DIMENSION
            && u64::from(header.stride) == u64::from(header.width) * 4
            && u64::from(header.payload_len) == u64::from(header.stride) * u64::from(header.height);
        if !fits {
            return Err(WireError::Geometry {
                width: header.width,
                height: header.height,
                stride: header.stride,
                payload_len: header.payload_len,
            });
        }
        Ok(header)
    }
}

/// A viewer → VMM message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerMessage {
    /// Opening message with the viewer's wire version.
    Hello {
        /// The viewer's [`VERSION`].
        version: u16,
    },
    /// Frame `sequence` is on screen.
    Presented {
        /// [`FrameHeader::sequence`] of the frame shown.
        sequence: u64,
    },
}

impl ViewerMessage {
    /// The 16-byte wire form.
    pub fn encode(&self) -> [u8; MESSAGE_LEN] {
        let (kind, value) = match *self {
            Self::Hello { version } => (KIND_HELLO, u64::from(version)),
            Self::Presented { sequence } => (KIND_PRESENTED, sequence),
        };
        let mut out = [0u8; MESSAGE_LEN];
        out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        out[4..6].copy_from_slice(&VERSION.to_le_bytes());
        out[6..8].copy_from_slice(&kind.to_le_bytes());
        out[8..16].copy_from_slice(&value.to_le_bytes());
        out
    }

    /// Parse one message.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let kind = check_preamble("viewer message", bytes, MESSAGE_LEN)?;
        let value = le64(bytes, 8);
        match kind {
            KIND_HELLO => Ok(Self::Hello {
                version: u16::try_from(value).unwrap_or(u16::MAX),
            }),
            KIND_PRESENTED => Ok(Self::Presented { sequence: value }),
            kind => Err(WireError::Kind { kind }),
        }
    }
}

/// Check length, magic and version; return the kind.
fn check_preamble(what: &'static str, bytes: &[u8], need: usize) -> Result<u16, WireError> {
    if bytes.len() < need {
        return Err(WireError::Short {
            what,
            len: bytes.len(),
            need,
        });
    }
    let magic = le32(bytes, 0);
    if magic != MAGIC {
        return Err(WireError::BadMagic { found: magic });
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != VERSION {
        return Err(WireError::Version { found: version });
    }
    Ok(u16::from_le_bytes([bytes[6], bytes[7]]))
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(word)
}

#[cfg(test)]
#[path = "present_protocol_tests.rs"]
mod tests;
