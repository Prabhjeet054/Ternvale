//! Host controller command ring.

use crate::event::{self, EventRing, CC_SUCCESS, CC_TRB_ERROR};
use crate::guest_mem::{GuestMemoryAccess, MemError};
use crate::slots::SlotTable;
use crate::trb::{
    TRB_TYPE_ADDRESS_DEVICE, TRB_TYPE_CONFIGURE_EP, TRB_TYPE_DISABLE_SLOT, TRB_TYPE_ENABLE_SLOT,
    TRB_TYPE_LINK, TRB_TYPE_NOOP,
};

/// Command ring processor state.
#[derive(Debug, Clone, Default)]
pub struct CommandRing {
    /// Dequeue pointer (guest GPA).
    pub dequeue: u64,
    /// Producer cycle.
    pub cycle: bool,
    /// Running (CRCR low bit / USBCMD run).
    pub running: bool,
}

impl CommandRing {
    /// Process TRBs until a link with IOC or a command that stops the ring.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::usb",
        skip(self, mem, slots, events)
    )]
    pub fn run(
        &mut self,
        mem: &mut dyn GuestMemoryAccess,
        slots: &mut SlotTable,
        events: &mut EventRing,
    ) -> Result<(), MemError> {
        if !self.running || self.dequeue == 0 {
            return Ok(());
        }
        let mut index = 0u32;
        let mut ptr = self.dequeue;
        for _ in 0..32 {
            let trb = event::trace_command_trb(mem, ptr, index)?;
            if trb.cycle() != self.cycle {
                tracing::trace!(target: "ternvale::usb", "command ring cycle mismatch; stopping");
                break;
            }
            let cmd_ptr = ptr;
            match trb.trb_type() {
                TRB_TYPE_ENABLE_SLOT => {
                    let slot_id = slots.enable_slot().unwrap_or(0);
                    if slot_id == 0 {
                        events.post_command(mem, 0, CC_TRB_ERROR, cmd_ptr)?;
                    } else {
                        events.post_command(mem, slot_id, CC_SUCCESS, cmd_ptr)?;
                    }
                }
                TRB_TYPE_DISABLE_SLOT => {
                    let slot_id = ((trb.control >> 24) & 0xff) as u8;
                    if slots.disable_slot(slot_id) {
                        tracing::info!(target: "ternvale::usb", slot_id, "xhci slot disabled");
                        events.post_command(mem, slot_id, CC_SUCCESS, cmd_ptr)?;
                    } else {
                        events.post_command(mem, 0, CC_TRB_ERROR, cmd_ptr)?;
                    }
                }
                TRB_TYPE_ADDRESS_DEVICE => {
                    let slot_id = ((trb.control >> 24) & 0xff) as u8;
                    if slots.slot_mut(slot_id).is_some() {
                        tracing::info!(target: "ternvale::usb", slot_id, "xhci address device");
                        events.post_command(mem, slot_id, CC_SUCCESS, cmd_ptr)?;
                    } else {
                        events.post_command(mem, 0, CC_TRB_ERROR, cmd_ptr)?;
                    }
                }
                TRB_TYPE_CONFIGURE_EP => {
                    let slot_id = ((trb.control >> 24) & 0xff) as u8;
                    if let Some(slot) = slots.slot_mut(slot_id) {
                        slot.ep0.enabled = true;
                        slot.ep0.dequeue = trb.param;
                        slot.ep0.cycle = true;
                        slot.ep_in.enabled = true;
                        slot.ep_in.dequeue = trb.param;
                        slot.ep_in.cycle = true;
                        tracing::info!(target: "ternvale::usb", slot_id, "xhci configure endpoint");
                        events.post_command(mem, slot_id, CC_SUCCESS, cmd_ptr)?;
                    } else {
                        events.post_command(mem, 0, CC_TRB_ERROR, cmd_ptr)?;
                    }
                }
                TRB_TYPE_NOOP => {
                    events.post_command(mem, 0, CC_SUCCESS, cmd_ptr)?;
                }
                TRB_TYPE_LINK => {
                    ptr = trb.param & !0xF;
                    if trb.control & (1 << 1) != 0 {
                        self.cycle = !self.cycle;
                    }
                    index += 1;
                    continue;
                }
                other => {
                    tracing::warn!(target: "ternvale::usb", ty = other, "unhandled command trb");
                    events.post_command(mem, 0, CC_TRB_ERROR, cmd_ptr)?;
                }
            }
            ptr += 16;
            index += 1;
        }
        self.dequeue = ptr;
        Ok(())
    }
}
