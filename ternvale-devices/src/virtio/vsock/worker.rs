//! vsock worker: TX chains into the muxer, muxer packets into RX chains.
//!
//! Wakes on every queue notify and every [`POLL`] so host Unix streams are read
//! and accepted without a guest kick. The event queue is bound but unused: no
//! `VIRTIO_VSOCK_EVENT_TRANSPORT_RESET` is ever sent.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ternvale_vmm::GuestMemory;

use super::muxer::Muxer;
use super::packet::{self, Header, HDR_LEN, MAX_PAYLOAD};
use super::{VsockStats, EVENT_QUEUE, RX_QUEUE, TX_QUEUE};
use crate::virtio::irq::VirtioIrq;
use crate::virtio::queue::{Chain, SplitQueue};
use crate::virtio::{QueueNotify, VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC};

const POLL: Duration = Duration::from_millis(5);
const INT_VRING: u32 = 1;

pub(super) enum Kick {
    Notify(QueueNotify),
    Reset,
}

struct Bound {
    key: (u16, u64, u64, u64),
    queue: SplitQueue,
}

pub(super) struct Worker {
    muxer: Muxer,
    memory: Arc<Mutex<GuestMemory>>,
    irq: Arc<VirtioIrq>,
    stats: Arc<VsockStats>,
    rx: Option<Bound>,
    tx: Option<Bound>,
}

impl Worker {
    pub(super) fn new(
        muxer: Muxer,
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
        stats: Arc<VsockStats>,
    ) -> Self {
        Self {
            muxer,
            memory,
            irq,
            stats,
            rx: None,
            tx: None,
        }
    }

    pub(super) fn run(mut self, kicks: Receiver<Kick>) {
        tracing::debug!(target: "ternvale::virtio::vsock", "virtio-vsock worker started");
        loop {
            match kicks.recv_timeout(POLL) {
                Ok(Kick::Notify(notify)) => self.bind(&notify),
                Ok(Kick::Reset) => {
                    self.rx = None;
                    self.tx = None;
                    self.muxer.reset();
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.process_tx();
            self.muxer.poll_host(Instant::now());
            self.deliver_rx();
        }
        tracing::debug!(
            target: "ternvale::virtio::vsock",
            open = self.muxer.connections(),
            "virtio-vsock worker stopped"
        );
    }

    fn bind(&mut self, notify: &QueueNotify) {
        let slot = match notify.index {
            RX_QUEUE => &mut self.rx,
            TX_QUEUE => &mut self.tx,
            EVENT_QUEUE => return,
            other => {
                tracing::warn!(
                    target: "ternvale::virtio::vsock",
                    queue = other,
                    "notify for a queue virtio-vsock does not have"
                );
                return;
            }
        };
        let key = (notify.size, notify.desc, notify.avail, notify.used);
        if slot.as_ref().is_some_and(|bound| bound.key == key) {
            return;
        }
        tracing::debug!(
            target: "ternvale::virtio::vsock",
            queue = notify.index,
            size = notify.size,
            desc = %format!("{:#x}", notify.desc),
            "virtio-vsock queue bound"
        );
        *slot = Some(Bound {
            key,
            queue: SplitQueue::new(
                notify.size,
                notify.desc,
                notify.avail,
                notify.used,
                notify.features & VIRTIO_F_INDIRECT_DESC != 0,
                notify.features & VIRTIO_F_EVENT_IDX != 0,
            ),
        });
    }

    fn lock(&self) -> MutexGuard<'_, GuestMemory> {
        match self.memory.lock() {
            Ok(guard) => guard,
            Err(poison) => poison.into_inner(),
        }
    }

    fn process_tx(&mut self) {
        let Some(mut bound) = self.tx.take() else {
            return;
        };
        let mut packets = Vec::new();
        {
            let mut mem = self.lock();
            let chains: Vec<Chain> = bound.queue.chains(&mem).collect();
            let mut completed = false;
            for chain in chains {
                match read_tx(&mem, &chain) {
                    Some(p) => packets.push(p),
                    None => {
                        self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
                completed |= bound.queue.add_used(&mut mem, chain.head, 0);
            }
            if completed && bound.queue.notification_needed(&mem) {
                self.irq.raise(INT_VRING);
            }
        }
        self.tx = Some(bound);
        for (hdr, payload) in packets {
            packet::trace("tx", &hdr);
            self.stats.tx_packets.fetch_add(1, Ordering::Relaxed);
            self.stats
                .tx_bytes
                .fetch_add(payload.len() as u64, Ordering::Relaxed);
            self.muxer.handle_guest(&hdr, &payload);
        }
    }

    fn deliver_rx(&mut self) {
        let Some(mut bound) = self.rx.take() else {
            return;
        };
        let mut completed = false;
        let memory = Arc::clone(&self.memory);
        while self.muxer.has_rx() {
            let mut mem = match memory.lock() {
                Ok(guard) => guard,
                Err(poison) => poison.into_inner(),
            };
            let Some(chain) = bound.queue.chains(&mem).next() else {
                tracing::trace!(target: "ternvale::virtio::vsock", "no rx buffer; holding packets");
                break;
            };
            let capacity: usize = chain.writable.iter().map(|b| b.len as usize).sum();
            if !chain.readable.is_empty() || capacity <= HDR_LEN {
                tracing::warn!(
                    target: "ternvale::virtio::vsock",
                    head = chain.head,
                    capacity,
                    "unusable vsock rx buffer; completing empty"
                );
                completed |= bound.queue.add_used(&mut mem, chain.head, 0);
                continue;
            }
            let max_payload = (capacity - HDR_LEN).min(MAX_PAYLOAD);
            let Some((hdr, payload)) = self.muxer.next_rx(max_payload, Instant::now()) else {
                completed |= bound.queue.add_used(&mut mem, chain.head, 0);
                break;
            };
            let used = write_rx(&mut mem, &chain, &hdr, &payload);
            completed |= bound.queue.add_used(&mut mem, chain.head, used);
            drop(mem);
            if used > 0 {
                packet::trace("rx", &hdr);
                self.stats.rx_packets.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .rx_bytes
                    .fetch_add(payload.len() as u64, Ordering::Relaxed);
            } else {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        if completed && bound.queue.notification_needed(&self.lock()) {
            self.irq.raise(INT_VRING);
        }
        self.rx = Some(bound);
    }
}

/// Gather a TX chain into a header and its payload. `None` on a malformed chain.
pub(super) fn read_tx(mem: &GuestMemory, chain: &Chain) -> Option<(Header, Vec<u8>)> {
    if !chain.writable.is_empty() {
        return reject(chain.head, "tx chain has device-writable buffers");
    }
    let total: usize = chain
        .readable
        .iter()
        .fold(0usize, |acc, b| acc.saturating_add(b.len as usize));
    if !(HDR_LEN..=HDR_LEN + MAX_PAYLOAD).contains(&total) {
        return reject(chain.head, "tx chain length outside header..header+64KiB");
    }
    let mut bytes = vec![0u8; total];
    let mut off = 0;
    for buf in &chain.readable {
        let len = buf.len as usize;
        if mem
            .read_bytes(buf.addr, &mut bytes[off..off + len])
            .is_err()
        {
            return reject(chain.head, "tx buffer is outside guest memory");
        }
        off += len;
    }
    let hdr = Header::parse(&bytes)?;
    if hdr.len as usize != total - HDR_LEN {
        tracing::warn!(
            target: "ternvale::virtio::vsock",
            head = chain.head,
            hdr_len = hdr.len,
            payload = total - HDR_LEN,
            "vsock header length does not match the chain; dropped"
        );
        return None;
    }
    Some((hdr, bytes.split_off(HDR_LEN)))
}

/// Write header plus payload across the RX chain's writable buffers.
pub(super) fn write_rx(mem: &mut GuestMemory, chain: &Chain, hdr: &Header, payload: &[u8]) -> u32 {
    let mut bytes = Vec::with_capacity(HDR_LEN + payload.len());
    bytes.extend_from_slice(&hdr.encode());
    bytes.extend_from_slice(payload);
    let mut off = 0usize;
    for buf in &chain.writable {
        if off == bytes.len() {
            break;
        }
        let take = (buf.len as usize).min(bytes.len() - off);
        if mem.write_bytes(buf.addr, &bytes[off..off + take]).is_err() {
            tracing::warn!(
                target: "ternvale::virtio::vsock",
                head = chain.head,
                addr = %format!("{:#x}", buf.addr),
                "vsock rx buffer is outside guest memory; packet dropped"
            );
            return 0;
        }
        off += take;
    }
    // At most HDR_LEN + MAX_PAYLOAD bytes.
    bytes.len() as u32
}

fn reject<T>(head: u16, reason: &'static str) -> Option<T> {
    tracing::warn!(target: "ternvale::virtio::vsock", head, reason, "vsock tx chain dropped");
    None
}
