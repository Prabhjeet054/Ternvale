//! Walking a function's capability list the way a guest does.
//!
//! The list starts at the byte at [`CAP_PTR`] when status bit 4 is set. Each
//! entry is `id, next` followed by its body, and `next == 0` ends the list.
//! The low two bits of every pointer are reserved and masked off, as Linux
//! does. A pointer back into the 64-byte header, or one already visited, is a
//! malformed list; the walk reports it instead of looping.

use super::config::{ConfigSpace, CAP_PTR, STATUS, STATUS_CAP_LIST};
use super::PciError;

/// Capabilities live after the 64-byte type 0 header.
pub const CAP_SPACE_START: u8 = 0x40;

/// One entry of the capability list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    /// Config-space offset of the capability ID byte.
    pub offset: u8,
    /// Capability ID, for example 0x09 (vendor) or 0x11 (MSI-X).
    pub id: u8,
    /// Offset of the next capability, 0 at the end of the list.
    pub next: u8,
}

impl ConfigSpace {
    /// Every capability in list order. Empty when status bit 4 is clear.
    pub fn capabilities(&self) -> Result<Vec<Capability>, PciError> {
        if self.u16_at(STATUS) & STATUS_CAP_LIST == 0 {
            return Ok(Vec::new());
        }
        let mut caps: Vec<Capability> = Vec::new();
        let mut at = self.read(CAP_PTR as u16, 1) as u8 & !3;
        while at != 0 {
            if at < CAP_SPACE_START {
                return Err(PciError::CapabilityList {
                    offset: at,
                    reason: "pointer into the type 0 header",
                });
            }
            if caps.iter().any(|cap| cap.offset == at) {
                return Err(PciError::CapabilityList {
                    offset: at,
                    reason: "pointer loops back to an earlier capability",
                });
            }
            let id = self.read(u16::from(at), 1) as u8;
            let next = self.read(u16::from(at) + 1, 1) as u8 & !3;
            caps.push(Capability {
                offset: at,
                id,
                next,
            });
            at = next;
        }
        Ok(caps)
    }
}
