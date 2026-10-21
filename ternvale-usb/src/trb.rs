//! xHCI transfer request blocks (TRB). See xHCI §6.4.

/// TRB size in bytes.
pub const TRB_SIZE: usize = 16;

/// TRB type field: `TRB_TYPE_*` in bits 15:10 of the control dword.
pub const TRB_TYPE_NORMAL: u8 = 1;
pub const TRB_TYPE_SETUP_STAGE: u8 = 2;
pub const TRB_TYPE_DATA_STAGE: u8 = 3;
pub const TRB_TYPE_STATUS_STAGE: u8 = 4;
pub const TRB_TYPE_ISOCH: u8 = 5;
pub const TRB_TYPE_LINK: u8 = 6;
pub const TRB_TYPE_EVENT_DATA: u8 = 7;
pub const TRB_TYPE_NOOP: u8 = 8;
pub const TRB_TYPE_ENABLE_SLOT: u8 = 9;
pub const TRB_TYPE_DISABLE_SLOT: u8 = 10;
pub const TRB_TYPE_ADDRESS_DEVICE: u8 = 11;
pub const TRB_TYPE_CONFIGURE_EP: u8 = 12;
pub const TRB_TYPE_EVALUATE_CONTEXT: u8 = 13;
pub const TRB_TYPE_RESET_EP: u8 = 14;
pub const TRB_TYPE_STOP_EP: u8 = 15;
pub const TRB_TYPE_SET_TR_DEQUEUE: u8 = 16;
pub const TRB_TYPE_RESET_DEVICE: u8 = 17;
pub const TRB_TYPE_FORCE_EVENT: u8 = 18;
pub const TRB_TYPE_NEGOTIATE_BW: u8 = 19;
pub const TRB_TYPE_SET_LATENCY: u8 = 20;
pub const TRB_TYPE_GET_PORT_BW: u8 = 21;
pub const TRB_TYPE_VENDOR: u8 = 31;

/// One TRB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Trb {
    /// Parameter / buffer pointer.
    pub param: u64,
    /// Status (length, completion code, etc.).
    pub status: u32,
    /// Control (cycle, type, flags).
    pub control: u32,
}

impl Trb {
    /// Parse 16 little-endian bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < TRB_SIZE {
            return None;
        }
        let param = u64::from_le_bytes(bytes[0..8].try_into().ok()?);
        let status = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
        let control = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
        Some(Self {
            param,
            status,
            control,
        })
    }

    /// Serialize to 16 bytes.
    pub fn to_bytes(self) -> [u8; TRB_SIZE] {
        let mut out = [0u8; TRB_SIZE];
        out[0..8].copy_from_slice(&self.param.to_le_bytes());
        out[8..12].copy_from_slice(&self.status.to_le_bytes());
        out[12..16].copy_from_slice(&self.control.to_le_bytes());
        out
    }

    /// TRB type (bits 15:10).
    pub fn trb_type(self) -> u8 {
        ((self.control >> 10) & 0x3f) as u8
    }

    /// Cycle bit (control bit 0).
    pub fn cycle(self) -> bool {
        self.control & 1 != 0
    }

    /// Log this TRB at TRACE (`ternvale::usb`).
    #[tracing::instrument(level = "trace", target = "ternvale::usb", skip(self))]
    pub fn trace(self, ring: &str, index: u32) {
        let kind = type_name(self.trb_type());
        tracing::trace!(
            target: "ternvale::usb",
            ring,
            index,
            trb_type = kind,
            ty = self.trb_type(),
            param = format!("{:#x}", self.param),
            status = format!("{:#x}", self.status),
            control = format!("{:#x}", self.control),
            cycle = self.cycle(),
            "xhci trb"
        );
    }
}

/// Human-readable TRB type for logs.
pub fn type_name(ty: u8) -> &'static str {
    match ty {
        TRB_TYPE_NORMAL => "NORMAL",
        TRB_TYPE_SETUP_STAGE => "SETUP_STAGE",
        TRB_TYPE_DATA_STAGE => "DATA_STAGE",
        TRB_TYPE_STATUS_STAGE => "STATUS_STAGE",
        TRB_TYPE_ISOCH => "ISOCH",
        TRB_TYPE_LINK => "LINK",
        TRB_TYPE_EVENT_DATA => "EVENT_DATA",
        TRB_TYPE_NOOP => "NOOP",
        TRB_TYPE_ENABLE_SLOT => "ENABLE_SLOT",
        TRB_TYPE_DISABLE_SLOT => "DISABLE_SLOT",
        TRB_TYPE_ADDRESS_DEVICE => "ADDRESS_DEVICE",
        TRB_TYPE_CONFIGURE_EP => "CONFIGURE_EP",
        TRB_TYPE_EVALUATE_CONTEXT => "EVALUATE_CONTEXT",
        TRB_TYPE_RESET_EP => "RESET_EP",
        TRB_TYPE_STOP_EP => "STOP_EP",
        TRB_TYPE_SET_TR_DEQUEUE => "SET_TR_DEQUEUE",
        TRB_TYPE_RESET_DEVICE => "RESET_DEVICE",
        TRB_TYPE_FORCE_EVENT => "FORCE_EVENT",
        TRB_TYPE_NEGOTIATE_BW => "NEGOTIATE_BW",
        TRB_TYPE_SET_LATENCY => "SET_LATENCY",
        TRB_TYPE_GET_PORT_BW => "GET_PORT_BW",
        TRB_TYPE_VENDOR => "VENDOR",
        _ => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_type_field() {
        let trb = Trb {
            param: 0xdead,
            status: 8,
            control: (TRB_TYPE_SETUP_STAGE as u32) << 10 | 1,
        };
        let parsed = Trb::from_bytes(&trb.to_bytes()).expect("parse");
        assert_eq!(parsed, trb);
        assert_eq!(parsed.trb_type(), TRB_TYPE_SETUP_STAGE);
        assert_eq!(type_name(parsed.trb_type()), "SETUP_STAGE");
    }
}
