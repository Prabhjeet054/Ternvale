//! Inflate/deflate/stats/reporting virtqueue handling.

use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;

use ternvale_vmm::GuestMemory;

use super::guest_stats::GuestMem;
use super::policy::IdleSample;
use super::{Inner, MAX_PFNS, STAT_SIZE};

const Q_INFLATE: u16 = 0;
const Q_DEFLATE: u16 = 1;
const Q_STATS: u16 = 2;
const Q_REPORTING: u16 = 3;
const NQUEUE: u16 = 4;
const INT_VRING: u32 = 1;

use super::super::queue::SplitQueue;
use super::super::{QueueNotify, VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC};

pub(super) enum Kick {
    Notify(QueueNotify),
    Refresh,
    Reset,
}

pub(super) fn worker_loop(rx: mpsc::Receiver<Kick>, inner: Arc<Inner>) {
    let mut sample = IdleSample::new();
    while let Ok(kick) = rx.recv() {
        match kick {
            Kick::Notify(notify) => inner.serve(notify),
            Kick::Refresh => {
                let target = inner.policy.refresh(&inner.guest, &mut sample);
                inner.set_target(target);
            }
            Kick::Reset => {
                *inner.queues.lock().unwrap_or_else(|p| p.into_inner()) =
                    std::array::from_fn(|_| None);
            }
        }
    }
}

impl Inner {
    pub(super) fn serve(&self, notify: QueueNotify) {
        if notify.index >= NQUEUE {
            tracing::warn!(
                target: "ternvale::balloon",
                queue = notify.index,
                "balloon notify for unknown queue"
            );
            return;
        }
        let key = (notify.size, notify.desc, notify.avail, notify.used);
        let mut held = self.queues.lock().unwrap_or_else(|p| p.into_inner());
        let slot = &mut held[usize::from(notify.index)];
        if slot.as_ref().is_none_or(|(bound, _)| *bound != key) {
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
        let Some((_, queue)) = slot.as_mut() else {
            return;
        };
        let mut mem = ternvale_vmm::lockwatch::lock(&self.memory, "guest-memory");
        let chains: Vec<_> = queue.chains(&mem).collect();
        let mut completed = false;
        for chain in chains {
            match notify.index {
                Q_INFLATE => {
                    let pfns = read_pfns(&mem, &chain);
                    self.inflated.inflate(&mem, &pfns);
                }
                Q_DEFLATE => {
                    let pfns = read_pfns(&mem, &chain);
                    self.inflated.deflate(&mem, &pfns);
                }
                Q_STATS => parse_stats(&mem, &chain, &self.guest),
                Q_REPORTING => {
                    for buf in chain.readable.iter().chain(chain.writable.iter()) {
                        self.inflated
                            .report_unused(&mem, buf.addr, buf.len as usize);
                    }
                }
                _ => {}
            }
            completed |= queue.add_used(&mut mem, chain.head, 0);
        }
        self.actual
            .store(self.inflated.len() as u32, Ordering::Relaxed);
        if completed && queue.notification_needed(&mem) {
            self.irq.raise(INT_VRING);
        }
    }
}

fn read_pfns(mem: &GuestMemory, chain: &super::super::Chain) -> Vec<u32> {
    let mut out = Vec::new();
    for buf in &chain.readable {
        if buf.len == 0 {
            continue;
        }
        let mut bytes = vec![0u8; buf.len as usize];
        if mem.read_bytes(buf.addr, &mut bytes).is_err() {
            tracing::warn!(
                target: "ternvale::balloon",
                addr = format!("{:#x}", buf.addr),
                "balloon PFN buffer is outside guest memory"
            );
            continue;
        }
        for chunk in bytes.chunks_exact(4) {
            if out.len() >= MAX_PFNS {
                tracing::warn!(target: "ternvale::balloon", "balloon PFN list truncated");
                return out;
            }
            out.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
    }
    out
}

fn parse_stats(mem: &GuestMemory, chain: &super::super::Chain, guest: &GuestMem) {
    for buf in &chain.readable {
        let mut bytes = vec![0u8; buf.len as usize];
        if mem.read_bytes(buf.addr, &mut bytes).is_err() {
            continue;
        }
        for rec in bytes.chunks_exact(STAT_SIZE) {
            let tag = u16::from_le_bytes([rec[0], rec[1]]);
            let mut val_bytes = [0u8; 8];
            val_bytes.copy_from_slice(&rec[2..10]);
            guest.update(tag, u64::from_le_bytes(val_bytes));
        }
    }
}
