//! Event ring (interrupter 0).

use crate::guest_mem::{GuestMemoryAccess, MemError};
use crate::mem::{read_trb, write_trb};
use crate::trb::{Trb, TRB_SIZE};

/// Completion codes posted in event TRBs (xHCI table 6-7).
pub const CC_SUCCESS: u32 = 1;
pub const CC_TRB_ERROR: u32 = 5;
pub const CC_STALL_ERROR: u32 = 6;

/// Event ring state for one interrupter.
#[derive(Debug, Clone, Default)]
pub struct EventRing {
    /// `ERSTBA` base.
    pub erst_base: u64,
    /// Number of entries in the ERST.
    pub erst_size: u16,
    /// Producer index.
    pub index: u32,
    /// Cycle state.
    pub cycle: bool,
}

impl EventRing {
    /// Post one event TRB. Type is always transfer event (32) for simplicity.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::usb",
        skip(self, mem),
        fields(slot, ep, code)
    )]
    pub fn post(
        &mut self,
        mem: &mut dyn GuestMemoryAccess,
        slot: u8,
        ep: u8,
        code: u32,
        trb_pointer: u64,
        length: u32,
    ) -> Result<(), MemError> {
        if self.erst_base == 0 || self.erst_size == 0 {
            tracing::warn!(target: "ternvale::usb", "event ring not configured");
            return Ok(());
        }
        let gpa = self.erst_base + u64::from(self.index) * TRB_SIZE as u64;
        // Transfer event TRB type = 32
        let control = (32u32 << 10) | u32::from(self.cycle);
        let status = (code & 0xff) | (length << 16);
        let param = trb_pointer;
        let trb = Trb {
            param,
            status,
            control,
        };
        trb.trace("event", self.index);
        write_trb(mem, gpa, trb)?;
        self.index = (self.index + 1) % u32::from(self.erst_size);
        if self.index == 0 {
            self.cycle = !self.cycle;
        }
        tracing::debug!(
            target: "ternvale::usb",
            slot,
            ep,
            completion = code,
            len = length,
            index = self.index,
            "xhci event posted"
        );
        Ok(())
    }

    /// Post a command completion event (type 33).
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip(self, mem))]
    pub fn post_command(
        &mut self,
        mem: &mut dyn GuestMemoryAccess,
        slot: u8,
        code: u32,
        trb_pointer: u64,
    ) -> Result<(), MemError> {
        if self.erst_base == 0 {
            return Ok(());
        }
        let gpa = self.erst_base + u64::from(self.index) * TRB_SIZE as u64;
        let control = (33u32 << 10) | u32::from(self.cycle);
        let status = code & 0xff;
        let trb = Trb {
            param: trb_pointer,
            status,
            control,
        };
        trb.trace("event-cmd", self.index);
        write_trb(mem, gpa, trb)?;
        self.index = (self.index + 1) % u32::from(self.erst_size.max(1));
        if self.index == 0 {
            self.cycle = !self.cycle;
        }
        tracing::debug!(
            target: "ternvale::usb",
            slot,
            completion = code,
            "xhci command completion event"
        );
        Ok(())
    }
}

/// Read a TRB from the command ring at `gpa` and log it.
pub fn trace_command_trb(
    mem: &dyn GuestMemoryAccess,
    gpa: u64,
    index: u32,
) -> Result<Trb, MemError> {
    let trb = read_trb(mem, gpa)?;
    trb.trace("command", index);
    Ok(trb)
}
