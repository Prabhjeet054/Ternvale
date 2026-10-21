//! Device transfer rings (control and bulk).

use std::sync::Arc;

use crate::event::{EventRing, CC_STALL_ERROR, CC_SUCCESS};
use crate::guest_mem::{GuestMemoryAccess, MemError};
use crate::hid::{config_bundle, device_descriptor, HidKind};
use crate::host::UsbControlSetup;
use crate::input_ctx::dci_to_usb_endpoint;
use crate::mem::{read_bytes, read_trb, write_bytes};
use crate::passthrough::{ControlJob, PassthroughWorker, PipeJob};
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
    skip(mem, slot, events, passthrough),
    fields(slot_id, dci)
)]
pub fn process_transfer(
    mem: &mut dyn GuestMemoryAccess,
    slot_id: u8,
    dci: u8,
    slot: &mut Slot,
    events: &mut EventRing,
    passthrough: Option<&Arc<PassthroughWorker>>,
) -> Result<(), MemError> {
    if slot.passthrough && dci == 0 && slot.ep0_passthrough_busy {
        tracing::trace!(target: "ternvale::usb", slot_id, "ep0 passthrough transfer still in flight");
        return Ok(());
    }
    let ep_enabled = if dci == 0 {
        true
    } else {
        slot.ep_ring(dci).enabled
    };
    if !ep_enabled && dci != 0 {
        tracing::warn!(target: "ternvale::usb", slot_id, dci, "transfer on disabled endpoint");
        return Ok(());
    }
    if slot.passthrough && dci != 0 && slot.ep_ring(dci).passthrough_busy {
        tracing::trace!(target: "ternvale::usb", slot_id, dci, "passthrough pipe transfer in flight");
        return Ok(());
    }
    let (mut ptr, mut cycle) = if dci == 0 {
        (slot.ep0.dequeue, slot.ep0.cycle)
    } else {
        let ring = slot.ep_ring(dci);
        (ring.dequeue, ring.cycle)
    };
    let mut hold_dequeue = false;
    for step in 0..16 {
        let trb = crate::mem::read_trb(mem, ptr)?;
        trb.trace("transfer", step);
        if trb.cycle() != cycle {
            break;
        }
        let trb_ptr = ptr;
        match trb.trb_type() {
            TRB_TYPE_SETUP_STAGE => {
                if handle_setup(mem, slot, trb, events, slot_id, trb_ptr, passthrough)? {
                    hold_dequeue = true;
                    break;
                }
            }
            TRB_TYPE_DATA_STAGE | TRB_TYPE_NORMAL => {
                if slot.passthrough && dci != 0 {
                    if forward_pipe(slot, slot_id, dci, trb, trb_ptr, passthrough) {
                        hold_dequeue = true;
                        break;
                    }
                } else if !slot.pending_in.is_empty() {
                    let mps = if dci == 0 {
                        trb.status as usize & 0xffff
                    } else {
                        slot.ep_ring(dci).max_packet.max(1) as usize
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
                if !slot.passthrough || dci != 0 {
                    events.post(mem, slot_id, 0, CC_SUCCESS, trb_ptr, 0)?;
                }
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
    if !hold_dequeue {
        if dci == 0 {
            slot.ep0.dequeue = ptr;
            slot.ep0.cycle = cycle;
        } else {
            let ring = slot.ep_ring_mut(dci);
            ring.dequeue = ptr;
            ring.cycle = cycle;
        }
    }
    Ok(())
}

fn forward_pipe(
    slot: &mut Slot,
    slot_id: u8,
    dci: u8,
    trb: Trb,
    trb_ptr: u64,
    passthrough: Option<&Arc<PassthroughWorker>>,
) -> bool {
    let endpoint = slot
        .endpoint_addr
        .get(dci as usize)
        .copied()
        .filter(|a| *a != 0)
        .or_else(|| dci_to_usb_endpoint(dci));
    if let (Some(worker), Some(endpoint)) = (passthrough, endpoint) {
        let length = trb.status as usize & 0xffff;
        let is_in = endpoint & 0x80 != 0;
        tracing::debug!(
            target: "ternvale::usb",
            slot_id,
            dci,
            endpoint = format!("{endpoint:#04x}"),
            length,
            "forwarding pipe transfer to host usb backend"
        );
        worker.submit_pipe(PipeJob {
            port: slot.port,
            slot_id,
            dci,
            endpoint_address: endpoint,
            trb_ptr,
            data_gpa: trb.param,
            length,
            is_in,
        });
        slot.ep_ring_mut(dci).passthrough_busy = true;
        return true;
    }
    false
}

/// Returns `true` when the EP0 ring must wait for the passthrough worker.
fn handle_setup(
    mem: &mut dyn GuestMemoryAccess,
    slot: &mut Slot,
    setup_trb: Trb,
    events: &mut EventRing,
    slot_id: u8,
    trb_ptr: u64,
    passthrough: Option<&Arc<PassthroughWorker>>,
) -> Result<bool, MemError> {
    let setup_bytes = read_bytes(mem, setup_trb.param, 8)?;
    if slot.passthrough {
        if let (Some(worker), Some(parsed)) =
            (passthrough, UsbControlSetup::from_bytes(&setup_bytes))
        {
            let mut data_out = Vec::new();
            if parsed.request_type & 0x80 == 0 && parsed.length > 0 {
                let data_trb = read_trb(mem, trb_ptr + 16)?;
                if data_trb.trb_type() == TRB_TYPE_DATA_STAGE {
                    let len = parsed.length as usize;
                    data_out = read_bytes(mem, data_trb.param, len)?;
                }
            }
            tracing::debug!(
                target: "ternvale::usb",
                slot_id,
                port = slot.port,
                req = parsed.request,
                "forwarding control setup to host usb backend"
            );
            worker.submit_control(ControlJob {
                port: slot.port,
                slot_id,
                setup: parsed,
                data_out,
                data_in_len: parsed.length as usize,
                setup_trb_ptr: trb_ptr,
            });
            slot.ep0_passthrough_busy = true;
            return Ok(true);
        }
        tracing::warn!(
            target: "ternvale::usb",
            slot_id,
            "passthrough slot without worker or invalid setup"
        );
        events.post(mem, slot_id, 0, CC_STALL_ERROR, trb_ptr, 0)?;
        return Ok(false);
    }
    let setup = setup_bytes;
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
        slot.pending_in = data[..take].to_vec();
    }
    events.post(mem, slot_id, 0, CC_SUCCESS, trb_ptr, 8)?;
    Ok(false)
}
