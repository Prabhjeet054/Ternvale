//! virtio-balloon (id 5): inflate/deflate, stats, free-page reporting.
//!
//! Layout follows virtio 1.2 §5.5. `num_pages` is the device target;
//! `actual` is written by the driver.

mod guest_stats;
mod pages;
mod policy;
mod queues;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ternvale_hv::{watch_memory_pressure, MemoryPressure, PressureWatch};
use ternvale_metrics::Metrics;
use ternvale_vmm::GuestMemory;

use super::irq::{IrqHook, VirtioIrq};
use super::queue::SplitQueue;
use super::{QueueNotify, VirtioDevice, VIRTIO_F_EVENT_IDX};
use guest_stats::GuestMem;
use pages::{Inflated, LibcReclaim, PageReclaim, PFN_SHIFT};
use policy::{IdleSample, Policy};
use queues::{worker_loop, Kick};

/// Virtio balloon device id.
pub const VIRTIO_BALLOON_ID: u32 = 5;
/// `VIRTIO_BALLOON_F_MUST_TELL_HOST`.
pub const F_MUST_TELL_HOST: u64 = 1 << 0;
/// `VIRTIO_BALLOON_F_STATS_VQ`.
pub const F_STATS_VQ: u64 = 1 << 1;
/// `VIRTIO_BALLOON_F_DEFLATE_ON_OOM`.
pub const F_DEFLATE_ON_OOM: u64 = 1 << 2;
/// `VIRTIO_BALLOON_F_PAGE_REPORTING` (no `FREE_PAGE_HINT`, so reporting is queue 3).
pub const F_PAGE_REPORTING: u64 = 1 << 5;

const NQUEUE: u16 = 4;
const INT_CONFIG: u32 = 2;
const MAX_PFNS: usize = 4096;
const STAT_SIZE: usize = 10;

/// Building a virtio-balloon device failed.
#[derive(Debug, thiserror::Error)]
pub enum BalloonError {
    /// The virtio-mmio slot is outside the platform window.
    #[error("virtio-balloon slot {slot} is outside the virtio-mmio window")]
    BadSlot {
        /// Requested slot.
        slot: u32,
    },
}

/// Base, size, MMIO device, and live snapshot from [`VirtioBalloon::attach`].
pub type AttachedBalloon = (
    u64,
    u64,
    Box<dyn ternvale_vmm::MmioDevice>,
    Arc<BalloonHandle>,
);

/// How the balloon is opened.
pub struct BalloonConfig {
    /// Fully deflated guest RAM, MiB.
    pub ram_mib: u64,
    /// Floor the balloon will not cross, MiB.
    pub ram_min_mib: u64,
    /// Step 61 counters (idle detection). Optional in unit tests.
    pub metrics: Option<Arc<Metrics>>,
    /// Guest vCPU count.
    pub vcpus: u32,
    /// Subscribe to `dispatch_source` memory pressure.
    pub watch_pressure: bool,
    /// Background target refresh (off in unit tests).
    pub auto_policy: bool,
    /// Host `madvise` implementation.
    pub reclaim: Option<Box<dyn PageReclaim>>,
}

/// Snapshot for `query-stats` (optional field; older clients ignore it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalloonSnapshot {
    /// Fully deflated guest RAM, MiB.
    pub ram_mib: u64,
    /// Balloon floor, MiB.
    pub ram_min_mib: u64,
    /// Device `num_pages` (4 KiB PFNs).
    pub target_pages: u64,
    /// Last `actual` (4 KiB PFNs).
    pub actual_pages: u64,
    /// `target_pages` in MiB.
    pub target_mib: u64,
    /// `actual_pages` in MiB.
    pub actual_mib: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_memtot_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_memfree_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_memavail_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_in_use_bytes: Option<u64>,
    /// Host `dispatch_source` MEMORYPRESSURE name.
    pub pressure: String,
    /// Guest vCPUs were idle over the last policy window.
    pub idle: bool,
    /// False when `balloon-set` holds a manual target.
    pub auto: bool,
}

/// Shared handle so `ternvale stats` can read balloon size without the MMIO device.
pub struct BalloonHandle {
    inner: Arc<Inner>,
    kick: Sender<Kick>,
}

impl BalloonHandle {
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn snapshot(&self) -> BalloonSnapshot {
        self.inner.snapshot()
    }

    /// Set `num_pages` from the control socket. Capped by floor and guest in-use.
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn set_target_mib(&self, mib: u64) -> BalloonSnapshot {
        let pages = self.inner.policy.set_manual_mib(mib, &self.inner.guest);
        self.inner.set_target(pages);
        if let Err(error) = self.kick.send(Kick::Refresh) {
            tracing::error!(target: "ternvale::balloon", error = %error, "balloon kick failed");
        }
        self.inner.snapshot()
    }

    /// Drop a manual target and resume host-pressure auto policy.
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn set_auto(&self) -> BalloonSnapshot {
        self.inner.policy.clear_manual();
        if let Err(error) = self.kick.send(Kick::Refresh) {
            tracing::error!(target: "ternvale::balloon", error = %error, "balloon kick failed");
        }
        self.inner.snapshot()
    }
}

type QueueBind = ((u16, u64, u64, u64), SplitQueue);

pub(super) struct Inner {
    memory: Arc<Mutex<GuestMemory>>,
    irq: Arc<VirtioIrq>,
    queues: Mutex<[Option<QueueBind>; NQUEUE as usize]>,
    inflated: Inflated,
    pub(super) guest: GuestMem,
    pub(super) policy: Policy,
    actual: AtomicU32,
    generation: AtomicU32,
    ram_mib: u64,
    ram_min_mib: u64,
}

/// virtio-balloon device.
pub struct VirtioBalloon {
    pub(super) inner: Arc<Inner>,
    kick: Option<Sender<Kick>>,
    worker: Option<JoinHandle<()>>,
    policy_thread: Option<JoinHandle<()>>,
    _pressure: Option<PressureWatch>,
}

impl VirtioBalloon {
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn new(
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
        config: BalloonConfig,
    ) -> Self {
        let reclaim = config.reclaim.unwrap_or_else(|| Box::new(LibcReclaim));
        let inner = Arc::new(Inner {
            memory,
            irq,
            queues: Mutex::new(std::array::from_fn(|_| None)),
            inflated: Inflated::new(reclaim),
            guest: GuestMem::default(),
            policy: Policy::new(
                config.ram_mib,
                config.ram_min_mib,
                config.metrics,
                config.vcpus,
            ),
            actual: AtomicU32::new(0),
            generation: AtomicU32::new(0),
            ram_mib: config.ram_mib,
            ram_min_mib: config.ram_min_mib,
        });
        let (tx, rx) = mpsc::channel();
        let worker_inner = Arc::clone(&inner);
        let worker = std::thread::Builder::new()
            .name("ternvale-balloon".into())
            .spawn(move || worker_loop(rx, worker_inner))
            .ok();
        let policy_thread = if config.auto_policy {
            let policy_tx = tx.clone();
            let policy_inner = Arc::clone(&inner);
            std::thread::Builder::new()
                .name("ternvale-balloon-policy".into())
                .spawn(move || {
                    let mut sample = IdleSample::new();
                    loop {
                        std::thread::sleep(Duration::from_secs(2));
                        let target = policy_inner
                            .policy
                            .refresh(&policy_inner.guest, &mut sample);
                        policy_inner.set_target(target);
                        if policy_tx.send(Kick::Refresh).is_err() {
                            break;
                        }
                    }
                })
                .ok()
        } else {
            None
        };
        let pressure = if config.watch_pressure {
            let inner = Arc::clone(&inner);
            let tx = tx.clone();
            watch_memory_pressure(Arc::new(move |level: MemoryPressure| {
                inner.policy.set_pressure(level);
                let _ = tx.send(Kick::Refresh);
            }))
        } else {
            None
        };
        tracing::info!(
            target: "ternvale::balloon",
            ram_mib = config.ram_mib,
            ram_min_mib = config.ram_min_mib,
            "virtio-balloon ready"
        );
        Self {
            inner,
            kick: Some(tx),
            worker,
            policy_thread,
            _pressure: pressure,
        }
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::balloon",
        skip(memory, irq_hook, config),
        fields(slot)
    )]
    pub fn attach(
        slot: u32,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
        config: BalloonConfig,
    ) -> Result<AttachedBalloon, BalloonError> {
        let base = super::slot_base(slot).ok_or_else(|| {
            tracing::error!(target: "ternvale::balloon", slot, "virtio-balloon slot rejected");
            BalloonError::BadSlot { slot }
        })?;
        let irq = VirtioIrq::new();
        irq.set_hook(irq_hook);
        let balloon = Self::new(memory, Arc::clone(&irq), config);
        let kick = balloon.kick.clone().ok_or_else(|| {
            tracing::error!(target: "ternvale::balloon", slot, "balloon worker channel missing");
            BalloonError::BadSlot { slot }
        })?;
        let handle = Arc::new(BalloonHandle {
            inner: Arc::clone(&balloon.inner),
            kick,
        });
        let transport = super::mmio::VirtioMmio::with_irq(slot, Box::new(balloon), irq);
        Ok((
            base,
            ternvale_vmm::VIRTIO_MMIO_SLOT_SIZE,
            Box::new(transport),
            handle,
        ))
    }

    fn send(&self, kick: Kick) {
        if let Some(tx) = &self.kick {
            if let Err(error) = tx.send(kick) {
                tracing::error!(target: "ternvale::balloon", error = %error, "balloon kick failed");
            }
        }
    }
}

impl Drop for VirtioBalloon {
    fn drop(&mut self) {
        self.kick.take();
        if let Some(join) = self.worker.take() {
            let _ = join.join();
        }
        if let Some(join) = self.policy_thread.take() {
            let _ = join.join();
        }
    }
}

impl Inner {
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub(super) fn set_target(&self, pages: u32) {
        let prev = self.policy.target.swap(pages, Ordering::AcqRel);
        if prev != pages {
            self.generation.fetch_add(1, Ordering::AcqRel);
            self.irq.raise(INT_CONFIG);
            tracing::info!(
                target: "ternvale::balloon",
                num_pages = pages,
                "balloon target changed"
            );
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub(super) fn snapshot(&self) -> BalloonSnapshot {
        let target = u64::from(self.policy.target.load(Ordering::Relaxed));
        let actual = u64::from(self.actual.load(Ordering::Relaxed));
        let tot = self.guest.memtot.load(Ordering::Relaxed);
        BalloonSnapshot {
            ram_mib: self.ram_mib,
            ram_min_mib: self.ram_min_mib,
            target_pages: target,
            actual_pages: actual,
            target_mib: (target * (1 << PFN_SHIFT)) >> 20,
            actual_mib: (actual * (1 << PFN_SHIFT)) >> 20,
            guest_memtot_bytes: self.guest.has_tot().then_some(tot),
            guest_memfree_bytes: self
                .guest
                .has_tot()
                .then_some(self.guest.memfree.load(Ordering::Relaxed)),
            guest_memavail_bytes: self
                .guest
                .has_tot()
                .then_some(self.guest.avail.load(Ordering::Relaxed)),
            guest_in_use_bytes: self.guest.in_use_bytes(),
            pressure: self.policy.pressure().as_str().to_string(),
            idle: self.policy.idle.load(Ordering::Relaxed) != 0,
            auto: !self.policy.is_manual(),
        }
    }
}

impl VirtioDevice for VirtioBalloon {
    fn device_id(&self) -> u32 {
        VIRTIO_BALLOON_ID
    }

    fn device_features(&self) -> u64 {
        F_MUST_TELL_HOST | F_STATS_VQ | F_DEFLATE_ON_OOM | F_PAGE_REPORTING | VIRTIO_F_EVENT_IDX
    }

    fn num_queues(&self) -> u16 {
        NQUEUE
    }

    fn read_config(&self, offset: u64, size: u8) -> u64 {
        let num = self.inner.policy.target.load(Ordering::Relaxed);
        let actual = self.inner.actual.load(Ordering::Relaxed);
        let mut raw = [0u8; 8];
        raw[..4].copy_from_slice(&num.to_le_bytes());
        raw[4..].copy_from_slice(&actual.to_le_bytes());
        let start = offset as usize;
        let n = usize::from(size);
        if start >= raw.len() {
            return 0;
        }
        let end = (start + n).min(raw.len());
        let mut buf = [0u8; 8];
        buf[..end - start].copy_from_slice(&raw[start..end]);
        u64::from_le_bytes(buf)
    }

    fn write_config(&self, offset: u64, size: u8, value: u64) {
        if offset == 4 && size == 4 {
            self.inner.actual.store(value as u32, Ordering::Relaxed);
            tracing::debug!(
                target: "ternvale::balloon",
                actual = value as u32,
                "guest wrote balloon actual"
            );
        }
    }

    fn config_generation(&self) -> u32 {
        self.inner.generation.load(Ordering::Relaxed)
    }

    fn notify(&self, queue: QueueNotify) {
        self.send(Kick::Notify(queue));
    }

    fn status_changed(&self, status: u32) {
        if status == 0 {
            self.send(Kick::Reset);
        }
    }
}

#[cfg(test)]
#[path = "balloon_tests.rs"]
mod tests;
