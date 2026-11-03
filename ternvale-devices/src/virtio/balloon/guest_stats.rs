//! Guest memory statistics from the balloon stats virtqueue.

use std::sync::atomic::{AtomicU64, Ordering};

/// `VIRTIO_BALLOON_S_MEMFREE` — virtio 1.2 Table 5.5.6.
pub const S_MEMFREE: u16 = 4;
/// `VIRTIO_BALLOON_S_MEMTOT`.
pub const S_MEMTOT: u16 = 5;
/// `VIRTIO_BALLOON_S_AVAIL`.
pub const S_AVAIL: u16 = 6;
/// `VIRTIO_BALLOON_S_CACHES`.
pub const S_CACHES: u16 = 7;

const TAG_MAX: u16 = 16;

/// Last stats the guest published (bytes). Missing tags stay 0.
#[derive(Debug, Default)]
pub struct GuestMem {
    pub memfree: AtomicU64,
    pub memtot: AtomicU64,
    pub avail: AtomicU64,
    pub caches: AtomicU64,
    pub seen: AtomicU64,
}

impl GuestMem {
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn update(&self, tag: u16, val: u64) {
        if tag > TAG_MAX {
            tracing::warn!(target: "ternvale::balloon", tag, "unknown balloon stat tag");
            return;
        }
        self.seen.fetch_or(1u64 << tag, Ordering::Relaxed);
        match tag {
            S_MEMFREE => self.memfree.store(val, Ordering::Relaxed),
            S_MEMTOT => self.memtot.store(val, Ordering::Relaxed),
            S_AVAIL => self.avail.store(val, Ordering::Relaxed),
            S_CACHES => self.caches.store(val, Ordering::Relaxed),
            _ => {}
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn has_tot(&self) -> bool {
        self.seen.load(Ordering::Relaxed) & (1 << S_MEMTOT) != 0
    }

    /// Bytes the guest is using. Prefers `memtot - avail`, else `memtot - memfree`.
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn in_use_bytes(&self) -> Option<u64> {
        if !self.has_tot() {
            return None;
        }
        let tot = self.memtot.load(Ordering::Relaxed);
        let seen = self.seen.load(Ordering::Relaxed);
        let free = if seen & (1 << S_AVAIL) != 0 {
            self.avail.load(Ordering::Relaxed)
        } else {
            self.memfree.load(Ordering::Relaxed)
        };
        Some(tot.saturating_sub(free))
    }
}
