//! Lock-free virtqueue kick: MMIO `QueueNotify` publishes from atomics and
//! calls [`VirtioDevice::notify`] without taking the transport mutex.

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::Arc;

use ternvale_vmm::KickPort;

use super::core::Queue;
use super::{QueueNotify, VirtioDevice};

/// virtio-mmio `QueueNotify` offset (virtio 1.2 §4.2.2).
const QUEUE_NOTIFY: u64 = 0x050;

struct Snap {
    ready: AtomicBool,
    size: AtomicU16,
    desc: AtomicU64,
    avail: AtomicU64,
    used: AtomicU64,
    features: AtomicU64,
}

impl Snap {
    fn empty() -> Self {
        Self {
            ready: AtomicBool::new(false),
            size: AtomicU16::new(0),
            desc: AtomicU64::new(0),
            avail: AtomicU64::new(0),
            used: AtomicU64::new(0),
            features: AtomicU64::new(0),
        }
    }
}

/// Shared with [`crate::virtio::VirtioMmio`]: ready-queue snapshots plus the device.
pub(crate) struct NotifyHub {
    snaps: Box<[Snap]>,
    device: Arc<dyn VirtioDevice>,
}

impl NotifyHub {
    pub(crate) fn new(queues: u16, device: Arc<dyn VirtioDevice>) -> Arc<Self> {
        let snaps = (0..queues).map(|_| Snap::empty()).collect();
        Arc::new(Self { snaps, device })
    }

    pub(crate) fn publish(&self, index: u32, queue: &Queue, features: u64) {
        let Some(snap) = self.snaps.get(index as usize) else {
            return;
        };
        snap.size.store(queue.num as u16, Ordering::Relaxed);
        snap.desc.store(queue.desc, Ordering::Relaxed);
        snap.avail.store(queue.driver, Ordering::Relaxed);
        snap.used.store(queue.device, Ordering::Relaxed);
        snap.features.store(features, Ordering::Relaxed);
        snap.ready.store(queue.ready, Ordering::Release);
    }

    pub(crate) fn clear(&self) {
        for snap in self.snaps.iter() {
            snap.ready.store(false, Ordering::Release);
        }
    }

    fn kick_index(&self, index: u32) -> bool {
        let Some(snap) = self.snaps.get(index as usize) else {
            return false;
        };
        if !snap.ready.load(Ordering::Acquire) {
            return false;
        }
        let notify = QueueNotify {
            index: index as u16,
            size: snap.size.load(Ordering::Relaxed),
            desc: snap.desc.load(Ordering::Relaxed),
            avail: snap.avail.load(Ordering::Relaxed),
            used: snap.used.load(Ordering::Relaxed),
            features: snap.features.load(Ordering::Relaxed),
        };
        tracing::trace!(
            target: "ternvale::virtio::transport",
            queue = index,
            "virtio queue notify (lock-free)"
        );
        self.device.notify(notify);
        true
    }
}

impl KickPort for NotifyHub {
    fn try_kick(&self, offset: u64, val: u64) -> bool {
        if offset != QUEUE_NOTIFY {
            return false;
        }
        self.kick_index(val as u32)
    }
}
