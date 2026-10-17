//! virtio-input configuration space (virtio 1.2 §5.8.4): the driver writes
//! `select`/`subsel`, then reads `size` and up to 128 bytes of the union.

use super::codes::{
    ABS_MAX_VALUE, ABS_X, ABS_Y, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, EV_ABS, EV_KEY, EV_REL, EV_REP,
    EV_SYN, REL_HWHEEL, REL_WHEEL,
};
use super::keymap;

/// `VIRTIO_INPUT_CFG_UNSET`.
pub const CFG_UNSET: u8 = 0x00;
/// `VIRTIO_INPUT_CFG_ID_NAME`.
pub const CFG_ID_NAME: u8 = 0x01;
/// `VIRTIO_INPUT_CFG_ID_SERIAL`.
pub const CFG_ID_SERIAL: u8 = 0x02;
/// `VIRTIO_INPUT_CFG_ID_DEVIDS`.
pub const CFG_ID_DEVIDS: u8 = 0x03;
/// `VIRTIO_INPUT_CFG_PROP_BITS`.
pub const CFG_PROP_BITS: u8 = 0x10;
/// `VIRTIO_INPUT_CFG_EV_BITS`.
pub const CFG_EV_BITS: u8 = 0x11;
/// `VIRTIO_INPUT_CFG_ABS_INFO`.
pub const CFG_ABS_INFO: u8 = 0x12;
/// Offset of the union within config space.
pub const UNION_OFFSET: u64 = 8;
/// Size of the union.
pub const UNION_LEN: usize = 128;
/// Total config space size.
pub const CONFIG_LEN: u64 = UNION_OFFSET + UNION_LEN as u64;

/// `BUS_VIRTUAL` (`include/uapi/linux/input.h`).
const BUS_VIRTUAL: u16 = 0x06;
/// Vendor/product IDs as QEMU's virtio-input devices report them, so guest
/// tooling treats them like any other virtio keyboard/tablet.
const VENDOR: u16 = 0x0627;

/// What the device presents as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// Keys with autorepeat.
    Keyboard,
    /// Absolute pointer (`ABS_X`/`ABS_Y` 0..=32767) with three buttons and
    /// both wheels.
    Tablet,
}

impl InputKind {
    /// Short name for logs.
    pub fn short(self) -> &'static str {
        match self {
            Self::Keyboard => "keyboard",
            Self::Tablet => "tablet",
        }
    }

    /// `ID_NAME` (what the guest shows as the device name).
    pub fn name(self) -> &'static str {
        match self {
            Self::Keyboard => "Ternvale Virtio Keyboard",
            Self::Tablet => "Ternvale Virtio Tablet",
        }
    }

    fn product(self) -> u16 {
        match self {
            Self::Keyboard => 0x0001,
            Self::Tablet => 0x0003,
        }
    }

    /// Codes advertised for `ev_type` (empty = type not supported).
    fn codes(self, ev_type: u16) -> Vec<u16> {
        match (self, ev_type) {
            (Self::Keyboard, EV_KEY) => keymap::linux_keys().collect(),
            (Self::Tablet, EV_KEY) => vec![BTN_LEFT, BTN_RIGHT, BTN_MIDDLE],
            (Self::Tablet, EV_ABS) => vec![ABS_X, ABS_Y],
            (Self::Tablet, EV_REL) => vec![REL_HWHEEL, REL_WHEEL],
            _ => Vec::new(),
        }
    }

    /// Whether the device may emit this event.
    pub fn supports(self, ev_type: u16, code: u16) -> bool {
        ev_type == EV_SYN || self.codes(ev_type).contains(&code)
    }
}

/// The union contents for `select`/`subsel` (empty = size 0).
pub fn payload(kind: InputKind, select: u8, subsel: u8) -> Vec<u8> {
    match (select, subsel) {
        (CFG_ID_NAME, 0) => kind.name().as_bytes().to_vec(),
        (CFG_ID_SERIAL, 0) => format!("ternvale-{}", kind.short()).into_bytes(),
        (CFG_ID_DEVIDS, 0) => [BUS_VIRTUAL, VENDOR, kind.product(), 0x0001]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect(),
        (CFG_EV_BITS, ty) => ev_bits(kind, u16::from(ty)),
        (CFG_ABS_INFO, axis) if kind == InputKind::Tablet && u16::from(axis) <= ABS_Y => {
            // struct virtio_input_absinfo { le32 min, max, fuzz, flat, res }.
            [0, ABS_MAX_VALUE, 0, 0, 0]
                .iter()
                .flat_map(|v: &u32| v.to_le_bytes())
                .collect()
        }
        _ => Vec::new(),
    }
}

fn ev_bits(kind: InputKind, ev_type: u16) -> Vec<u8> {
    if kind == InputKind::Keyboard && ev_type == EV_REP {
        // Linux only checks that the size is non-zero.
        return vec![0x03];
    }
    bitmap(&kind.codes(ev_type))
}

/// Little-endian bitmap, truncated after the highest set bit.
fn bitmap(codes: &[u16]) -> Vec<u8> {
    let Some(max) = codes.iter().max() else {
        return Vec::new();
    };
    let mut out = vec![0u8; usize::from(*max) / 8 + 1];
    for code in codes {
        out[usize::from(*code) / 8] |= 1 << (code % 8);
    }
    out
}
