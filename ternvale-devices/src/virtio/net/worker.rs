//! virtio-net worker: TX chains to the backend, backend frames into RX chains.
//!
//! The worker wakes on every queue notify and every [`POLL`] so frames from an
//! asynchronous backend (and kicks the driver chose not to send) are picked up.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ternvale_vmm::GuestMemory;

use super::backend::NetBackend;
use super::pcap::PcapWriter;
use super::{packet, NetStats, MAX_FRAME, NET_HDR_LEN, RX_QUEUE, TX_QUEUE};
use crate::virtio::irq::VirtioIrq;
use crate::virtio::queue::{Chain, SplitQueue};
use crate::virtio::{QueueNotify, VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC};

const POLL: Duration = Duration::from_millis(10);
const INT_VRING: u32 = 1;
const ETH_HLEN: usize = 14;
const HDR_F_NEEDS_CSUM: u8 = 1;
const GSO_NONE: u8 = 0;

pub(super) enum Kick {
    Notify(QueueNotify),
    Reset,
}

struct Bound {
    key: (u16, u64, u64, u64),
    queue: SplitQueue,
}

pub(super) struct Worker {
    backend: Box<dyn NetBackend>,
    memory: Arc<Mutex<GuestMemory>>,
    irq: Arc<VirtioIrq>,
    stats: Arc<NetStats>,
    pcap: Option<PcapWriter>,
    rx: Option<Bound>,
    tx: Option<Bound>,
    pending: Option<Vec<u8>>,
}

impl Worker {
    pub(super) fn new(
        backend: Box<dyn NetBackend>,
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
        stats: Arc<NetStats>,
        pcap: Option<PcapWriter>,
    ) -> Self {
        Self {
            backend,
            memory,
            irq,
            stats,
            pcap,
            rx: None,
            tx: None,
            pending: None,
        }
    }

    pub(super) fn run(mut self, kicks: Receiver<Kick>) {
        tracing::debug!(
            target: "ternvale::virtio::net",
            backend = self.backend.name(),
            "virtio-net worker started"
        );
        loop {
            match kicks.recv_timeout(POLL) {
                Ok(Kick::Notify(notify)) => self.bind(&notify),
                Ok(Kick::Reset) => {
                    self.rx = None;
                    self.tx = None;
                    if self.pending.take().is_some() {
                        tracing::debug!(
                            target: "ternvale::virtio::net",
                            "dropped held rx frame on reset"
                        );
                    }
                    tracing::debug!(target: "ternvale::virtio::net", "virtio-net queues reset");
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.process_tx();
            self.deliver_rx();
        }
        tracing::debug!(target: "ternvale::virtio::net", "virtio-net worker stopped");
    }

    fn bind(&mut self, notify: &QueueNotify) {
        let slot = match notify.index {
            RX_QUEUE => &mut self.rx,
            TX_QUEUE => &mut self.tx,
            other => {
                tracing::warn!(
                    target: "ternvale::virtio::net",
                    queue = other,
                    "notify for a queue virtio-net does not have"
                );
                return;
            }
        };
        let key = (notify.size, notify.desc, notify.avail, notify.used);
        if slot.as_ref().is_some_and(|bound| bound.key == key) {
            return;
        }
        tracing::debug!(
            target: "ternvale::virtio::net",
            queue = notify.index,
            size = notify.size,
            desc = %format!("{:#x}", notify.desc),
            "virtio-net queue bound"
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
        let mut frames = Vec::new();
        {
            let mut mem = self.lock();
            let chains: Vec<Chain> = bound.queue.chains(&mem).collect();
            let mut completed = false;
            for chain in chains {
                match read_tx(&mem, &chain) {
                    Some(frame) => frames.push(frame),
                    None => NetStats::add(&self.stats.tx_dropped, 1),
                }
                completed |= bound.queue.add_used(&mut mem, chain.head, 0);
            }
            if completed && bound.queue.notification_needed(&mem) {
                self.irq.raise(INT_VRING);
            }
        }
        self.tx = Some(bound);
        for frame in frames {
            packet::trace("tx", &frame);
            self.capture(&frame);
            match self.backend.send(&frame) {
                Ok(()) => {
                    NetStats::add(&self.stats.tx_packets, 1);
                    NetStats::add(&self.stats.tx_bytes, frame.len() as u64);
                }
                Err(error) => {
                    NetStats::add(&self.stats.tx_dropped, 1);
                    tracing::warn!(
                        target: "ternvale::net",
                        backend = self.backend.name(),
                        error = %error,
                        "backend refused tx frame"
                    );
                }
            }
        }
    }

    fn deliver_rx(&mut self) {
        let Some(mut bound) = self.rx.take() else {
            return;
        };
        let mut completed = false;
        loop {
            let frame = match self.pending.take() {
                Some(frame) => frame,
                None => match self.backend.recv() {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(error) => {
                        tracing::error!(
                            target: "ternvale::net",
                            backend = self.backend.name(),
                            error = %error,
                            "backend receive failed"
                        );
                        break;
                    }
                },
            };
            let mut mem = self.lock();
            let chain = bound.queue.chains(&mem).next();
            let Some(chain) = chain else {
                drop(mem);
                tracing::trace!(
                    target: "ternvale::virtio::net",
                    len = frame.len(),
                    "no rx buffer; holding frame"
                );
                self.pending = Some(frame);
                break;
            };
            let used = write_rx(&mut mem, &chain, &frame);
            completed |= bound.queue.add_used(&mut mem, chain.head, used);
            drop(mem);
            if used == 0 {
                NetStats::add(&self.stats.rx_dropped, 1);
                continue;
            }
            NetStats::add(&self.stats.rx_packets, 1);
            NetStats::add(&self.stats.rx_bytes, frame.len() as u64);
            packet::trace("rx", &frame);
            self.capture(&frame);
        }
        if completed && bound.queue.notification_needed(&self.lock()) {
            self.irq.raise(INT_VRING);
        }
        self.rx = Some(bound);
    }

    fn capture(&mut self, frame: &[u8]) {
        let Some(writer) = self.pcap.as_mut() else {
            return;
        };
        if let Err(error) = writer.write(frame) {
            tracing::error!(
                target: "ternvale::net",
                error = %error,
                "pcap write failed; capture disabled"
            );
            self.pcap = None;
        }
    }
}

/// Gather a TX chain into one frame, dropping the virtio header.
pub(super) fn read_tx(mem: &GuestMemory, chain: &Chain) -> Option<Vec<u8>> {
    if !chain.writable.is_empty() {
        return reject(chain.head, "tx chain has device-writable buffers");
    }
    let mut total = 0usize;
    for buf in &chain.readable {
        total = total.saturating_add(buf.len as usize);
    }
    if total > NET_HDR_LEN + MAX_FRAME {
        return reject(chain.head, "tx chain is longer than the largest frame");
    }
    if total < NET_HDR_LEN + ETH_HLEN {
        return reject(chain.head, "tx chain is shorter than header plus ethernet");
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
    let (flags, gso_type) = (bytes[0], bytes[1]);
    if flags & HDR_F_NEEDS_CSUM != 0 {
        return reject(chain.head, "tx header asks for checksum offload");
    }
    if gso_type != GSO_NONE {
        return reject(chain.head, "tx header asks for segmentation offload");
    }
    tracing::trace!(
        target: "ternvale::virtio::net",
        head = chain.head,
        len = total - NET_HDR_LEN,
        "virtio-net tx chain"
    );
    Some(bytes.split_off(NET_HDR_LEN))
}

/// Write header plus `frame` into an RX chain. Returns the used length, 0 on drop.
pub(super) fn write_rx(mem: &mut GuestMemory, chain: &Chain, frame: &[u8]) -> u32 {
    if !chain.readable.is_empty() {
        tracing::warn!(
            target: "ternvale::virtio::net",
            head = chain.head,
            "rx chain has device-readable buffers; dropping frame"
        );
        return 0;
    }
    let capacity: u64 = chain.writable.iter().map(|b| u64::from(b.len)).sum();
    let needed = NET_HDR_LEN + frame.len();
    if frame.len() > MAX_FRAME || capacity < needed as u64 {
        tracing::warn!(
            target: "ternvale::virtio::net",
            head = chain.head,
            capacity,
            needed,
            "rx buffer too small; dropping frame"
        );
        return 0;
    }
    let mut bytes = vec![0u8; needed];
    // flags, gso_type, hdr_len, gso_size, csum_start, csum_offset stay 0.
    // Without MRG_RXBUF the device reports num_buffers = 1.
    bytes[10..12].copy_from_slice(&1u16.to_le_bytes());
    bytes[NET_HDR_LEN..].copy_from_slice(frame);
    let mut off = 0usize;
    for buf in &chain.writable {
        if off == bytes.len() {
            break;
        }
        let take = (buf.len as usize).min(bytes.len() - off);
        if mem.write_bytes(buf.addr, &bytes[off..off + take]).is_err() {
            tracing::warn!(
                target: "ternvale::virtio::net",
                head = chain.head,
                addr = %format!("{:#x}", buf.addr),
                "rx buffer is outside guest memory; dropping frame"
            );
            return 0;
        }
        off += take;
    }
    tracing::trace!(
        target: "ternvale::virtio::net",
        head = chain.head,
        len = needed,
        "virtio-net rx chain"
    );
    needed as u32
}

fn reject(head: u16, reason: &'static str) -> Option<Vec<u8>> {
    tracing::warn!(target: "ternvale::virtio::net", head, reason, "tx frame dropped");
    None
}
