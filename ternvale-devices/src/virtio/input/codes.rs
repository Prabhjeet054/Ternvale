//! Linux input event codes (`include/uapi/linux/input-event-codes.h`) and the
//! 8-byte `virtio_input_event` (virtio 1.2 §5.8.6: le16 type, le16 code,
//! le32 value).

/// End of a report.
pub const EV_SYN: u16 = 0x00;
/// Key or button state.
pub const EV_KEY: u16 = 0x01;
/// Relative axis (wheel).
pub const EV_REL: u16 = 0x02;
/// Absolute axis (tablet position).
pub const EV_ABS: u16 = 0x03;
/// Autorepeat; advertising it makes the guest input core repeat held keys.
pub const EV_REP: u16 = 0x14;
/// `EV_SYN` code closing a report.
pub const SYN_REPORT: u16 = 0;
/// Horizontal wheel; positive is right.
pub const REL_HWHEEL: u16 = 0x06;
/// Vertical wheel; positive is away from the user (scroll up).
pub const REL_WHEEL: u16 = 0x08;
/// Absolute X.
pub const ABS_X: u16 = 0x00;
/// Absolute Y.
pub const ABS_Y: u16 = 0x01;
/// Left button.
pub const BTN_LEFT: u16 = 0x110;
/// Right button.
pub const BTN_RIGHT: u16 = 0x111;
/// Middle button.
pub const BTN_MIDDLE: u16 = 0x112;
/// Largest absolute coordinate the tablet reports (`ABS_X` / `ABS_Y` max).
pub const ABS_MAX_VALUE: u32 = 0x7fff;

/// One `virtio_input_event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputEvent {
    /// `EV_*`.
    pub ev_type: u16,
    /// `KEY_*`, `BTN_*`, `ABS_*`, `REL_*` or `SYN_*`.
    pub code: u16,
    /// 1 press / 0 release, a coordinate, or a signed delta as u32.
    pub value: u32,
}

impl InputEvent {
    /// Key or button press (`pressed`) or release.
    pub fn key(code: u16, pressed: bool) -> Self {
        Self {
            ev_type: EV_KEY,
            code,
            value: u32::from(pressed),
        }
    }

    /// Absolute axis position.
    pub fn abs(axis: u16, value: u32) -> Self {
        Self {
            ev_type: EV_ABS,
            code: axis,
            value,
        }
    }

    /// Relative axis delta.
    pub fn rel(axis: u16, delta: i32) -> Self {
        Self {
            ev_type: EV_REL,
            code: axis,
            value: delta as u32,
        }
    }

    /// `SYN_REPORT`.
    pub fn syn() -> Self {
        Self {
            ev_type: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        }
    }

    /// The 8 wire bytes.
    pub fn to_bytes(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0..2].copy_from_slice(&self.ev_type.to_le_bytes());
        out[2..4].copy_from_slice(&self.code.to_le_bytes());
        out[4..8].copy_from_slice(&self.value.to_le_bytes());
        out
    }

    /// Parse 8 wire bytes (the status queue).
    pub fn from_bytes(bytes: [u8; 8]) -> Self {
        Self {
            ev_type: u16::from_le_bytes([bytes[0], bytes[1]]),
            code: u16::from_le_bytes([bytes[2], bytes[3]]),
            value: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        }
    }
}

/// Short name of an `EV_*` type for logs.
pub fn type_name(ev_type: u16) -> &'static str {
    match ev_type {
        EV_SYN => "EV_SYN",
        EV_KEY => "EV_KEY",
        EV_REL => "EV_REL",
        EV_ABS => "EV_ABS",
        0x04 => "EV_MSC",
        0x11 => "EV_LED",
        EV_REP => "EV_REP",
        _ => "EV_?",
    }
}
