//! virtio-rng (entropy device, id 4): one request queue filled from `getentropy`.
//!
//! The device has no features and no config space. Each chain's device-writable
//! buffers are filled with host random bytes, up to [`RNG_MAX_PER_CHAIN`] per
//! chain, on the notifying vCPU thread. `getentropy` does not block.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ternvale_vmm::GuestMemory;

use super::irq::{IrqHook, VirtioIrq};
use super::queue::{Chain, SplitQueue};
use super::{QueueNotify, VirtioDevice, VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC};

/// Virtio entropy device id.
pub const VIRTIO_RNG_ID: u32 = 4;
/// Most bytes written into one request chain. Larger buffers are partly filled.
pub const RNG_MAX_PER_CHAIN: usize = 64 * 1024;

const INT_VRING: u32 = 1;

/// Building a virtio-rng device failed.
#[derive(Debug, thiserror::Error)]
pub enum RngError {
    /// The virtio-mmio slot is outside the platform window.
    #[error("virtio-rng slot {slot} is outside the virtio-mmio window")]
    BadSlot {
        /// Requested slot.
        slot: u32,
    },
}

/// Base, size, MMIO device, and live stats from [`VirtioRng::attach`].
pub type AttachedRng = (u64, u64, Box<dyn ternvale_vmm::MmioDevice>, Arc<RngStats>);

/// Counters for one virtio-rng device.
#[derive(Debug, Default)]
pub struct RngStats {
    /// Chains completed with random data.
    pub requests: AtomicU64,
    /// Random bytes written to the guest.
    pub bytes: AtomicU64,
    /// Chains completed empty (bad layout, bad address, or entropy failure).
    pub errors: AtomicU64,
}

impl RngStats {
    /// `(requests, bytes, errors)`.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::rng", skip_all)]
    pub fn snapshot(&self) -> [u64; 3] {
        [
            self.requests.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed),
        ]
    }
}

/// virtio-rng device state.
pub struct VirtioRng {
    memory: Arc<Mutex<GuestMemory>>,
    irq: Arc<VirtioIrq>,
    queue: Option<((u16, u64, u64, u64), SplitQueue)>,
    stats: Arc<RngStats>,
}

impl VirtioRng {
    /// Device over `memory` that raises `irq` when it completes requests.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::rng", skip_all)]
    pub fn new(memory: Arc<Mutex<GuestMemory>>, irq: Arc<VirtioIrq>) -> Self {
        tracing::info!(target: "ternvale::virtio::rng", "virtio-rng device ready");
        Self {
            memory,
            irq,
            queue: None,
            stats: Arc::new(RngStats::default()),
        }
    }

    /// Build a transport on `slot`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::rng",
        skip(memory, irq_hook),
        fields(slot)
    )]
    pub fn attach(
        slot: u32,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
    ) -> Result<AttachedRng, RngError> {
        let base = super::slot_base(slot).ok_or_else(|| {
            tracing::error!(target: "ternvale::virtio::rng", slot, "virtio-rng slot rejected");
            RngError::BadSlot { slot }
        })?;
        let irq = VirtioIrq::new();
        irq.set_hook(irq_hook);
        let rng = Self::new(memory, Arc::clone(&irq));
        let stats = rng.stats();
        let transport = super::mmio::VirtioMmio::with_irq(slot, Box::new(rng), irq);
        Ok((
            base,
            ternvale_vmm::VIRTIO_MMIO_SLOT_SIZE,
            Box::new(transport),
            stats,
        ))
    }

    /// Live counters.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::rng", skip_all)]
    pub fn stats(&self) -> Arc<RngStats> {
        Arc::clone(&self.stats)
    }

    fn lock(&self) -> ternvale_vmm::lockwatch::Guard<'_, GuestMemory> {
        ternvale_vmm::lockwatch::lock(&self.memory, "guest-memory")
    }

    fn serve(&mut self) {
        let Some((key, mut queue)) = self.queue.take() else {
            return;
        };
        let mut completed = false;
        {
            let mut mem = self.lock();
            let chains: Vec<Chain> = queue.chains(&mem).collect();
            for chain in chains {
                let used = fill_chain(&mut mem, &chain);
                match used {
                    Some(n) => {
                        self.stats.requests.fetch_add(1, Ordering::Relaxed);
                        self.stats.bytes.fetch_add(u64::from(n), Ordering::Relaxed);
                    }
                    None => {
                        self.stats.errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
                completed |= queue.add_used(&mut mem, chain.head, used.unwrap_or(0));
            }
            if completed && queue.notification_needed(&mem) {
                self.irq.raise(INT_VRING);
            }
        }
        self.queue = Some((key, queue));
    }
}

/// Fill the writable buffers of `chain`. `None` means the chain is completed empty.
fn fill_chain(mem: &mut GuestMemory, chain: &Chain) -> Option<u32> {
    if !chain.readable.is_empty() {
        tracing::warn!(
            target: "ternvale::virtio::rng",
            head = chain.head,
            "rng chain has device-readable buffers; completing empty"
        );
        return None;
    }
    let mut written = 0usize;
    for buf in &chain.writable {
        let take = (buf.len as usize).min(RNG_MAX_PER_CHAIN - written);
        if take == 0 {
            break;
        }
        let mut bytes = vec![0u8; take];
        if let Err(error) = ternvale_hv::fill_entropy(&mut bytes) {
            tracing::error!(
                target: "ternvale::virtio::rng",
                head = chain.head,
                error = %error,
                "host entropy unavailable; completing empty"
            );
            return None;
        }
        if mem.write_bytes(buf.addr, &bytes).is_err() {
            tracing::warn!(
                target: "ternvale::virtio::rng",
                head = chain.head,
                addr = %format!("{:#x}", buf.addr),
                len = take,
                "rng buffer is outside guest memory; completing empty"
            );
            return None;
        }
        written += take;
    }
    tracing::trace!(
        target: "ternvale::virtio::rng",
        head = chain.head,
        len = written,
        "virtio-rng request filled"
    );
    // `written <= RNG_MAX_PER_CHAIN`, so it fits in u32.
    Some(written as u32)
}

impl VirtioDevice for VirtioRng {
    fn device_id(&self) -> u32 {
        VIRTIO_RNG_ID
    }

    fn notify(&mut self, notify: QueueNotify) {
        if notify.index != 0 {
            tracing::warn!(
                target: "ternvale::virtio::rng",
                queue = notify.index,
                "notify for a queue virtio-rng does not have"
            );
            return;
        }
        let key = (notify.size, notify.desc, notify.avail, notify.used);
        if self.queue.as_ref().is_none_or(|(bound, _)| *bound != key) {
            tracing::debug!(
                target: "ternvale::virtio::rng",
                size = notify.size,
                desc = %format!("{:#x}", notify.desc),
                "virtio-rng queue bound"
            );
            let queue = SplitQueue::new(
                notify.size,
                notify.desc,
                notify.avail,
                notify.used,
                notify.features & VIRTIO_F_INDIRECT_DESC != 0,
                notify.features & VIRTIO_F_EVENT_IDX != 0,
            );
            self.queue = Some((key, queue));
        }
        self.serve();
    }

    fn status_changed(&mut self, status: u32) {
        if status == 0 && self.queue.take().is_some() {
            tracing::debug!(target: "ternvale::virtio::rng", "virtio-rng queue reset");
        }
    }
}

#[cfg(test)]
#[path = "rng_tests.rs"]
mod tests;
