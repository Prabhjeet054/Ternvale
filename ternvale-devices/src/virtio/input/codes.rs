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
/// Relative X (mouse).
pub const REL_X: u16 = 0x00;
/// Relative Y (mouse); positive is down.
pub const REL_Y: u16 = 0x01;
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
/// Gamepad south (Xbox A / Nintendo B).
pub const BTN_SOUTH: u16 = 0x130;
/// Gamepad east (Xbox B / Nintendo A).
pub const BTN_EAST: u16 = 0x131;
/// Gamepad north (Xbox Y / Nintendo X).
pub const BTN_NORTH: u16 = 0x133;
/// Gamepad west (Xbox X / Nintendo Y).
pub const BTN_WEST: u16 = 0x134;
/// Left bumper.
pub const BTN_TL: u16 = 0x136;
/// Right bumper.
pub const BTN_TR: u16 = 0x137;
/// Left trigger (digital).
pub const BTN_TL2: u16 = 0x138;
/// Right trigger (digital).
pub const BTN_TR2: u16 = 0x139;
/// Select / Back.
pub const BTN_SELECT: u16 = 0x13a;
/// Start.
pub const BTN_START: u16 = 0x13b;
/// Guide / mode.
pub const BTN_MODE: u16 = 0x13c;
/// Left stick click.
pub const BTN_THUMBL: u16 = 0x13d;
/// Right stick click.
pub const BTN_THUMBR: u16 = 0x13e;
/// Right stick X.
pub const ABS_RX: u16 = 0x03;
/// Right stick Y.
pub const ABS_RY: u16 = 0x04;
/// Left trigger analog (`ABS_Z`).
pub const ABS_Z: u16 = 0x02;
/// Right trigger analog (`ABS_RZ`).
pub const ABS_RZ: u16 = 0x05;
/// D-pad X (`-1..=1`).
pub const ABS_HAT0X: u16 = 0x10;
/// D-pad Y (`-1..=1`).
pub const ABS_HAT0Y: u16 = 0x11;
/// Largest absolute coordinate the tablet reports (`ABS_X` / `ABS_Y` max).
pub const ABS_MAX_VALUE: u32 = 0x7fff;
/// Stick range used for virtio-input gamepads.
pub const ABS_STICK_MAX: i32 = 32767;

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
