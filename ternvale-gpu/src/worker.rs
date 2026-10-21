//! The virtio-gpu worker thread: walks the control and cursor queues.
//!
//! The vCPU thread only forwards notifies here (the virtio-blk pattern from
//! Step 21), so a large `TRANSFER_TO_HOST_2D` copy never stalls the guest CPU.
//! Completion is signalled with the used-buffer interrupt.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use ternvale_devices::{Buffer, QueueNotify, SplitQueue, VirtioIrq};
use ternvale_devices::{VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC};
use ternvale_vmm::GuestMemory;

use crate::engine::GpuEngine;
use crate::protocol;

/// Queue 0: `controlq` (virtio 1.2 §5.7.2).
pub const CONTROL_QUEUE: u16 = 0;
/// Queue 1: `cursorq`. Present because drivers require it; commands are completed unprocessed.
pub const CURSOR_QUEUE: u16 = 1;
/// Largest request read from one chain: an attach-backing with the full
/// `MAX_BACKING_ENTRIES` is 24 + 8 + 16384 × 16 bytes.
pub const MAX_REQUEST: usize = 512 * 1024;

const INT_VRING: u32 = 1;

/// Messages from the transport (vCPU thread) to the worker.
pub enum Kick {
    /// The driver kicked a ready queue.
    Notify(QueueNotify),
    /// Device status went to 0.
    Reset,
}

type Bound = ((u16, u64, u64, u64), SplitQueue);

/// Worker loop; returns when the device is dropped.
pub fn run(
    rx: Receiver<Kick>,
    mut gpu: GpuEngine,
    memory: Arc<Mutex<GuestMemory>>,
    irq: Arc<VirtioIrq>,
) {
    let mut queues: [Option<Bound>; 2] = [None, None];
    while let Ok(kick) = rx.recv() {
        match kick {
            Kick::Reset => {
                queues = [None, None];
                gpu.reset();
            }
            Kick::Notify(notify) => {
                let Some(slot) = queues.get_mut(usize::from(notify.index)) else {
                    tracing::warn!(target: "ternvale::gpu", queue = notify.index, "notify for a queue virtio-gpu does not have");
                    continue;
                };
                let key = (notify.size, notify.desc, notify.avail, notify.used);
                if slot.as_ref().is_none_or(|(bound, _)| *bound != key) {
                    tracing::debug!(target: "ternvale::gpu", queue = notify.index, size = notify.size, desc = format!("{:#x}", notify.desc), "virtio-gpu queue bound");
                    let queue = SplitQueue::new(
                        notify.size,
                        notify.desc,
                        notify.avail,
                        notify.used,
                        notify.features & VIRTIO_F_INDIRECT_DESC != 0,
                        notify.features & VIRTIO_F_EVENT_IDX != 0,
                    );
                    *slot = Some((key, queue));
                }
                if let Some((_, queue)) = slot.as_mut() {
                    let mut mem = ternvale_vmm::lockwatch::lock(&memory, "guest-memory");
                    if serve(notify.index, queue, &mut mem, &mut gpu)
                        && queue.notification_needed(&mem)
                    {
                        irq.raise(INT_VRING);
                    }
                }
            }
        }
    }
    tracing::debug!(target: "ternvale::gpu", "virtio-gpu worker exiting");
}

/// Answer every published chain on `index`. `true` if any chain completed.
pub fn serve(
    index: u16,
    queue: &mut SplitQueue,
    mem: &mut GuestMemory,
    gpu: &mut GpuEngine,
) -> bool {
    let chains: Vec<_> = queue.chains(mem).collect();
    let mut completed = false;
    for chain in chains {
        let request = gather(mem, &chain.readable);
        let response = if index == CONTROL_QUEUE {
            gpu.handle(mem, &request)
        } else {
            cursor(&request);
            Vec::new()
        };
        let written = scatter(mem, &chain.writable, &response);
        tracing::trace!(target: "ternvale::gpu", queue = index, head = chain.head, request_len = request.len(), response_len = written, "virtio-gpu request completed");
        completed |= queue.add_used(mem, chain.head, written);
    }
    completed
}

fn cursor(request: &[u8]) {
    match protocol::parse_hdr(request) {
        Ok(hdr) => {
            tracing::debug!(target: "ternvale::gpu", cmd = protocol::type_name(hdr.kind), "virtio-gpu cursor command ignored (no cursor support yet)")
        }
        Err(error) => {
            tracing::warn!(target: "ternvale::gpu", error = %error, "malformed virtio-gpu cursor request")
        }
    }
}

/// The device-readable bytes of a chain, up to [`MAX_REQUEST`].
fn gather(mem: &GuestMemory, readable: &[Buffer]) -> Vec<u8> {
    let total: usize = readable.iter().map(|b| b.len as usize).sum();
    if total > MAX_REQUEST {
        tracing::warn!(target: "ternvale::gpu", total, max = MAX_REQUEST, "virtio-gpu request truncated");
    }
    let mut out = vec![0u8; total.min(MAX_REQUEST)];
    let mut at = 0usize;
    for buffer in readable {
        let take = (buffer.len as usize).min(out.len() - at);
        if take == 0 {
            continue;
        }
        if mem
            .read_bytes(buffer.addr, &mut out[at..at + take])
            .is_err()
        {
            tracing::warn!(target: "ternvale::gpu", addr = format!("{:#x}", buffer.addr), len = take, "virtio-gpu request buffer unreadable");
            out.truncate(at);
            return out;
        }
        at += take;
    }
    out
}

/// Write `response` across the writable buffers. Returns the bytes written.
fn scatter(mem: &mut GuestMemory, writable: &[Buffer], response: &[u8]) -> u32 {
    let mut at = 0usize;
    for buffer in writable {
        if at == response.len() {
            break;
        }
        let take = (buffer.len as usize).min(response.len() - at);
        if mem
            .write_bytes(buffer.addr, &response[at..at + take])
            .is_err()
        {
            tracing::warn!(target: "ternvale::gpu", addr = format!("{:#x}", buffer.addr), len = take, "virtio-gpu response buffer unwritable");
            break;
        }
        at += take;
    }
    if at < response.len() {
        tracing::warn!(target: "ternvale::gpu", response_len = response.len(), written = at, "virtio-gpu response did not fit the writable buffers");
    }
    // `response` is at most `DISPLAY_INFO_LEN` bytes.
    at as u32
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
