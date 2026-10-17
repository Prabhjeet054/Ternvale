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
//!   is on screen, and (version 2) the input messages of [`ViewerInput`].

/// `"TVDP"` read as a little-endian u32.
pub const MAGIC: u32 = u32::from_le_bytes(*b"TVDP");
/// Wire version; a peer with another version is refused. Version 2 added
/// input messages.
pub const VERSION: u16 = 2;
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
/// viewer → VMM: key; `value` = macOS virtual key code (bits 0..16) |
/// pressed (bit 16).
pub const KIND_KEY: u16 = 0x0201;
/// viewer → VMM: absolute pointer; `value` = x (bits 0..32) | y (bits 32..64),
/// each `0..=POINTER_MAX` across the displayed frame.
pub const KIND_POINTER: u16 = 0x0202;
/// viewer → VMM: button; `value` = button (bits 0..8: 0 left, 1 right,
/// 2 middle) | pressed (bit 8).
pub const KIND_BUTTON: u16 = 0x0203;
/// viewer → VMM: wheel clicks; `value` = dx as i32 (bits 0..32, + right) |
/// dy as i32 (bits 32..64, + up).
pub const KIND_SCROLL: u16 = 0x0204;
/// Largest pointer coordinate (matches the tablet's `ABS_X`/`ABS_Y` max).
pub const POINTER_MAX: u32 = 0x7fff;
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
    /// A known message whose value is out of range.
    #[error("message kind {kind:#06x} has invalid value {value:#x}")]
    Value {
        /// The type field.
        kind: u16,
        /// The value field.
        value: u64,
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
    /// Keyboard or pointer input for the guest.
    Input(ViewerInput),
}

/// Pointer button in a [`ViewerInput::Button`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    /// Primary.
    Left,
    /// Secondary.
    Right,
    /// Middle / third.
    Middle,
}

/// Input captured by the viewer window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerInput {
    /// A key went down or up. Host key repeats are not sent.
    Key {
        /// macOS virtual key code (`kVK_*`).
        mac_keycode: u16,
        /// Down (true) or up.
        pressed: bool,
    },
    /// Pointer moved, in `0..=POINTER_MAX` across the frame on screen.
    Pointer {
        /// Left to right.
        x: u32,
        /// Top to bottom.
        y: u32,
    },
    /// A button went down or up.
    Button {
        /// Which button.
        button: PointerButton,
        /// Down (true) or up.
        pressed: bool,
    },
    /// Wheel clicks.
    Scroll {
        /// Positive scrolls right.
        dx: i32,
        /// Positive scrolls up.
        dy: i32,
    },
}

impl ViewerInput {
    fn wire(self) -> (u16, u64) {
        match self {
            Self::Key {
                mac_keycode,
                pressed,
            } => (KIND_KEY, u64::from(mac_keycode) | u64::from(pressed) << 16),
            Self::Pointer { x, y } => (KIND_POINTER, u64::from(x) | u64::from(y) << 32),
            Self::Button { button, pressed } => {
                let index = match button {
                    PointerButton::Left => 0u64,
                    PointerButton::Right => 1,
                    PointerButton::Middle => 2,
                };
                (KIND_BUTTON, index | u64::from(pressed) << 8)
            }
            Self::Scroll { dx, dy } => (
                KIND_SCROLL,
                u64::from(dx as u32) | u64::from(dy as u32) << 32,
            ),
        }
    }

    fn parse(kind: u16, value: u64) -> Option<Result<Self, WireError>> {
        let bad = Err(WireError::Value { kind, value });
        let input = match kind {
            KIND_KEY if value >> 17 == 0 => Self::Key {
                mac_keycode: value as u16,
                pressed: value >> 16 & 1 == 1,
            },
            KIND_POINTER => {
                let (x, y) = (value as u32, (value >> 32) as u32);
                if x > POINTER_MAX || y > POINTER_MAX {
                    return Some(bad);
                }
                Self::Pointer { x, y }
            }
            KIND_BUTTON if value >> 9 == 0 => Self::Button {
                button: match value & 0xff {
                    0 => PointerButton::Left,
                    1 => PointerButton::Right,
                    2 => PointerButton::Middle,
                    _ => return Some(bad),
                },
                pressed: value >> 8 & 1 == 1,
            },
            KIND_SCROLL => Self::Scroll {
                dx: value as u32 as i32,
                dy: (value >> 32) as u32 as i32,
            },
            KIND_KEY | KIND_BUTTON => return Some(bad),
            _ => return None,
        };
        Some(Ok(input))
    }
}

impl ViewerMessage {
    /// The 16-byte wire form.
    pub fn encode(&self) -> [u8; MESSAGE_LEN] {
        let (kind, value) = match *self {
            Self::Hello { version } => (KIND_HELLO, u64::from(version)),
            Self::Presented { sequence } => (KIND_PRESENTED, sequence),
            Self::Input(input) => input.wire(),
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
            kind => match ViewerInput::parse(kind, value) {
                Some(input) => input.map(Self::Input),
                None => Err(WireError::Kind { kind }),
            },
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
