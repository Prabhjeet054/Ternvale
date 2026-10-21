//! xHCI input context parsing (minimal, for CONFIGURE_ENDPOINT).

use crate::guest_mem::{GuestMemoryAccess, MemError};
use crate::mem::read_bytes;
use crate::slots::Slot;

/// Device context size advertised via `HCCPARAMS1` CSZ (64-byte contexts).
const CONTEXT_SIZE: u64 = 64;

/// Apply endpoint rings from the guest input context at `input_context_gpa`.
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip(mem, slot))]
pub fn apply_input_context(
    mem: &mut dyn GuestMemoryAccess,
    input_context_gpa: u64,
    slot: &mut Slot,
) -> Result<(), MemError> {
    let icc = read_bytes(mem, input_context_gpa, 8)?;
    let add_flags = u32::from_le_bytes(icc[0..4].try_into().expect("icc"));
    for dci in 1..32u8 {
        if add_flags & (1u32 << dci) == 0 {
            continue;
        }
        let ctx_gpa = input_context_gpa + (1 + u64::from(dci)) * CONTEXT_SIZE;
        let ctx = read_bytes(mem, ctx_gpa, 64)?;
        let dequeue = u64::from_le_bytes(ctx[8..16].try_into().expect("dequeue"));
        let endpoint_addr = dci_to_usb_endpoint(dci);
        slot.set_endpoint_ring(dci, dequeue, true, endpoint_addr);
        tracing::debug!(
            target: "ternvale::usb",
            dci,
            dequeue = format!("{dequeue:#x}"),
            endpoint = endpoint_addr.map(|a| format!("{a:#04x}")),
            "input context endpoint configured"
        );
    }
    Ok(())
}

/// Map a device context index to a USB endpoint address (xHCI §4.8.3).
#[must_use]
pub fn dci_to_usb_endpoint(dci: u8) -> Option<u8> {
    if dci < 2 {
        return None;
    }
    let n = dci - 2;
    let ep_num = (n / 2) + 1;
    let dir = if n % 2 == 1 { 0x80 } else { 0 };
    Some(ep_num | dir)
}
