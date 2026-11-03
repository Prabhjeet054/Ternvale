//! Inflated PFN set and host-page `madvise` (16 KiB Apple Silicon pages).

use std::collections::BTreeSet;
use std::io;
use std::sync::Mutex;

use ternvale_vmm::{GuestMemory, MemAdvice, HOST_PAGE_SIZE};

/// Guest balloon PFNs are 4 KiB (virtio 1.2 §5.5.2 `VIRTIO_BALLOON_PFN_SHIFT`).
pub const PFN_SHIFT: u32 = 12;
pub const GUEST_PAGE: u64 = 1 << PFN_SHIFT;

/// Host `madvise` of ballooned guest RAM.
pub trait PageReclaim: Send + Sync {
    fn reusable(&self, mem: &GuestMemory, gpa: u64, len: usize) -> io::Result<()>;
    fn reuse(&self, mem: &GuestMemory, gpa: u64, len: usize) -> io::Result<()>;
}

/// `GuestMemory::advise` with Darwin `MADV_FREE_REUSABLE` / `MADV_FREE_REUSE`.
pub struct LibcReclaim;

impl PageReclaim for LibcReclaim {
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    fn reusable(&self, mem: &GuestMemory, gpa: u64, len: usize) -> io::Result<()> {
        mem.advise(gpa, len, MemAdvice::FreeReusable)
            .map_err(io::Error::other)
    }

    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    fn reuse(&self, mem: &GuestMemory, gpa: u64, len: usize) -> io::Result<()> {
        mem.advise(gpa, len, MemAdvice::FreeReuse)
            .map_err(io::Error::other)
    }
}

/// Counts `madvise` spans for unit tests.
#[derive(Clone, Default)]
pub struct CountingReclaim {
    pub reusable: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub reuse: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl PageReclaim for CountingReclaim {
    fn reusable(&self, _mem: &GuestMemory, _gpa: u64, _len: usize) -> io::Result<()> {
        self.reusable
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn reuse(&self, _mem: &GuestMemory, _gpa: u64, _len: usize) -> io::Result<()> {
        self.reuse.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// Inflated 4 KiB PFNs and host pages already marked reusable.
pub struct Inflated {
    pfns: Mutex<BTreeSet<u64>>,
    advised: Mutex<BTreeSet<u64>>,
    reclaim: Box<dyn PageReclaim>,
}

impl Inflated {
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn new(reclaim: Box<dyn PageReclaim>) -> Self {
        Self {
            pfns: Mutex::new(BTreeSet::new()),
            advised: Mutex::new(BTreeSet::new()),
            reclaim,
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn len(&self) -> u64 {
        self.pfns.lock().unwrap_or_else(|p| p.into_inner()).len() as u64
    }

    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn inflate(&self, mem: &GuestMemory, pfns: &[u32]) {
        let mut set = self.pfns.lock().unwrap_or_else(|p| p.into_inner());
        for pfn in pfns {
            let pfn = u64::from(*pfn);
            let gpa = pfn << PFN_SHIFT;
            if mem.host_ptr(gpa, GUEST_PAGE as usize).is_err() {
                tracing::warn!(
                    target: "ternvale::balloon",
                    pfn,
                    gpa = format!("{gpa:#x}"),
                    "inflate PFN is outside guest RAM"
                );
                continue;
            }
            set.insert(pfn);
        }
        drop(set);
        self.sync_host(mem);
    }

    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn deflate(&self, mem: &GuestMemory, pfns: &[u32]) {
        let mut set = self.pfns.lock().unwrap_or_else(|p| p.into_inner());
        for pfn in pfns {
            set.remove(&u64::from(*pfn));
        }
        drop(set);
        self.sync_host(mem);
    }

    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn report_unused(&self, mem: &GuestMemory, gpa: u64, len: usize) {
        let aligned = len as u64 & !(HOST_PAGE_SIZE - 1);
        if aligned < HOST_PAGE_SIZE || gpa % HOST_PAGE_SIZE != 0 {
            tracing::trace!(
                target: "ternvale::balloon",
                gpa = format!("{gpa:#x}"),
                len,
                "skip free-page report; not a host page"
            );
            return;
        }
        if let Err(error) = self.reclaim.reusable(mem, gpa, aligned as usize) {
            tracing::warn!(
                target: "ternvale::balloon",
                gpa = format!("{gpa:#x}"),
                len = aligned,
                error = %error,
                "free-page report madvise failed"
            );
        }
    }

    fn sync_host(&self, mem: &GuestMemory) {
        let pfns = self.pfns.lock().unwrap_or_else(|p| p.into_inner());
        let mut advised = self.advised.lock().unwrap_or_else(|p| p.into_inner());
        let mut want = BTreeSet::new();
        let per_host = HOST_PAGE_SIZE / GUEST_PAGE;
        for pfn in pfns.iter().copied() {
            let gpa = pfn << PFN_SHIFT;
            let host = gpa - (gpa % HOST_PAGE_SIZE);
            let base_pfn = host >> PFN_SHIFT;
            if (0..per_host).all(|i| pfns.contains(&(base_pfn + i))) {
                want.insert(host);
            }
        }
        for host in want.difference(&advised).copied().collect::<Vec<_>>() {
            match self.reclaim.reusable(mem, host, HOST_PAGE_SIZE as usize) {
                Ok(()) => {
                    advised.insert(host);
                }
                Err(error) => tracing::warn!(
                    target: "ternvale::balloon",
                    gpa = format!("{host:#x}"),
                    error = %error,
                    "inflate madvise failed"
                ),
            }
        }
        for host in advised.difference(&want).copied().collect::<Vec<_>>() {
            match self.reclaim.reuse(mem, host, HOST_PAGE_SIZE as usize) {
                Ok(()) => {
                    advised.remove(&host);
                }
                Err(error) => tracing::warn!(
                    target: "ternvale::balloon",
                    gpa = format!("{host:#x}"),
                    error = %error,
                    "deflate madvise failed"
                ),
            }
        }
    }
}
