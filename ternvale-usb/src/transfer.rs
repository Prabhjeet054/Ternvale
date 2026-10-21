//! Device transfer rings (control and bulk).

use crate::event::{EventRing, CC_STALL_ERROR, CC_SUCCESS};
use crate::guest_mem::{GuestMemoryAccess, MemError};
use crate::hid::{config_bundle, device_descriptor, HidKind};
use crate::mem::{read_bytes, write_bytes};
use crate::slots::Slot;
use crate::trb::{
    Trb, TRB_TYPE_DATA_STAGE, TRB_TYPE_LINK, TRB_TYPE_NORMAL, TRB_TYPE_SETUP_STAGE,
    TRB_TYPE_STATUS_STAGE,
};

const REQ_GET_DESCRIPTOR: u8 = 0x06;
const DESC_DEVICE: u8 = 1;
const DESC_CONFIG: u8 = 2;
const DESC_REPORT: u8 = 0x22;

/// Process one doorbell ring for `slot` endpoint `dci` (0 = default control).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::usb",
    skip(mem, slot, events),
    fields(slot_id, dci)
)]
pub fn process_transfer(
    mem: &mut dyn GuestMemoryAccess,
    slot_id: u8,
    dci: u8,
    slot: &mut Slot,
    events: &mut EventRing,
) -> Result<(), MemError> {
    let ep_enabled = if dci == 0 { true } else { slot.ep_in.enabled };
    if !ep_enabled && dci != 0 {
        tracing::warn!(target: "ternvale::usb", slot_id, dci, "transfer on disabled endpoint");
        return Ok(());
    }
    let (mut ptr, mut cycle) = if dci == 0 {
        (slot.ep0.dequeue, slot.ep0.cycle)
    } else {
        (slot.ep_in.dequeue, slot.ep_in.cycle)
    };
    for step in 0..16 {
        let trb = crate::mem::read_trb(mem, ptr)?;
        trb.trace("transfer", step);
        if trb.cycle() != cycle {
            break;
        }
        let trb_ptr = ptr;
        match trb.trb_type() {
            TRB_TYPE_SETUP_STAGE => {
                handle_setup(mem, slot, trb, events, slot_id, trb_ptr)?;
            }
            TRB_TYPE_DATA_STAGE | TRB_TYPE_NORMAL => {
                if !slot.pending_in.is_empty() {
                    let mps = if dci == 0 {
                        trb.status as usize & 0xffff
                    } else {
                        slot.ep_in.max_packet.max(1) as usize
                    };
                    let len = slot
                        .pending_in
                        .len()
                        .min(trb.status as usize & 0xffff)
                        .min(mps.max(1));
                    write_bytes(mem, trb.param, &slot.pending_in[..len])?;
                    slot.pending_in.clear();
                    events.post(mem, slot_id, dci, CC_SUCCESS, trb_ptr, len as u32)?;
                } else if trb.trb_type() == TRB_TYPE_NORMAL {
                    events.post(mem, slot_id, dci, CC_SUCCESS, trb_ptr, 0)?;
                }
            }
            TRB_TYPE_STATUS_STAGE => {
                events.post(mem, slot_id, 0, CC_SUCCESS, trb_ptr, 0)?;
            }
            TRB_TYPE_LINK => {
                ptr = trb.param & !0xF;
                if trb.control & (1 << 1) != 0 {
                    cycle = !cycle;
                }
                continue;
            }
            other => {
                tracing::warn!(target: "ternvale::usb", ty = other, "unhandled transfer trb");
                events.post(mem, slot_id, dci, CC_STALL_ERROR, trb_ptr, 0)?;
            }
        }
        ptr += 16;
    }
    if dci == 0 {
        slot.ep0.dequeue = ptr;
        slot.ep0.cycle = cycle;
    } else {
        slot.ep_in.dequeue = ptr;
        slot.ep_in.cycle = cycle;
    }
    Ok(())
}

fn handle_setup(
    mem: &mut dyn GuestMemoryAccess,
    slot: &mut Slot,
    setup_trb: Trb,
    events: &mut EventRing,
    slot_id: u8,
    trb_ptr: u64,
) -> Result<(), MemError> {
    let setup = read_bytes(mem, setup_trb.param, 8)?;
    let req_type = setup[0];
    let req = setup[1];
    let value = u16::from_le_bytes([setup[2], setup[3]]);
    let index = u16::from_le_bytes([setup[4], setup[5]]);
    let len = u16::from_le_bytes([setup[6], setup[7]]);
    tracing::debug!(
        target: "ternvale::usb",
        slot_id,
        req,
        req_type,
        value = format!("{value:#06x}"),
        index,
        len,
        "usb control setup"
    );
    let hid = slot.hid.unwrap_or(HidKind::Keyboard);
    let data: Vec<u8> = if req_type & 0x80 != 0 && req == REQ_GET_DESCRIPTOR {
        match (value >> 8) as u8 {
            DESC_DEVICE => device_descriptor(0x0627, 0x0001).to_vec(),
            DESC_CONFIG => config_bundle(hid, 8),
            DESC_REPORT => match hid {
                HidKind::Keyboard => crate::hid::BOOT_KEYBOARD_REPORT_DESC.to_vec(),
                HidKind::Mouse => crate::hid::BOOT_MOUSE_REPORT_DESC.to_vec(),
            },
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };
    if !data.is_empty() && len > 0 {
        let take = data.len().min(len as usize);
        // Data stage TRB is next; guest typically places buffer in following TRB — stash for DATA_STAGE handler.
        slot.pending_in = data[..take].to_vec();
    }
    events.post(mem, slot_id, 0, CC_SUCCESS, trb_ptr, 8)?;
    Ok(())
}
