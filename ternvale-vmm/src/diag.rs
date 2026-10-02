//! What a crash report needs from a running machine: the DTB the guest got,
//! the last MMIO accesses, and per-device access counts.
//!
//! [`MmioTrace`] is a fixed ring written by every vCPU without locks. Each
//! slot is a small seqlock: a writer claims the slot by swapping its even
//! sequence for the odd one of its event number, stores the fields, then
//! stores the even sequence; a reader keeps only slots whose sequence is the
//! expected even value before and after reading. A writer that finds its slot
//! claimed, or already holding a newer event, drops its event.

use std::sync::atomic::{fence, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// Events kept by [`MmioTrace::new`] callers that do not choose.
pub const MMIO_TRACE_LEN: usize = 512;

const UNMAPPED: u16 = u16::MAX;
const NO_CPU: u64 = u32::MAX as u64;

/// One recorded MMIO access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmioEvent {
    /// Event number since the VM started (0-based).
    pub seq: u64,
    /// Time since the trace was created, in microseconds.
    pub at_us: u64,
    /// Kernel vCPU id that took the exit, when known.
    pub cpu: Option<u32>,
    /// `true` for a guest store.
    pub write: bool,
    /// Guest physical address.
    pub gpa: u64,
    /// Access size in bytes.
    pub size: u8,
    /// Value written, or value returned to the guest.
    pub value: u64,
    /// Device name, or `unmapped`.
    pub device: String,
    /// Offset into the device window (the GPA when unmapped).
    pub offset: u64,
}

/// One MMIO window and how many accesses it has handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCount {
    /// Device name.
    pub name: String,
    /// Window base GPA.
    pub base: u64,
    /// Window length.
    pub size: u64,
    /// Accesses so far.
    pub accesses: u64,
}

struct Window {
    name: String,
    base: u64,
    size: u64,
    accesses: Arc<AtomicU64>,
}

#[derive(Default)]
struct Slot {
    seq: AtomicU64,
    at_ns: AtomicU64,
    gpa: AtomicU64,
    value: AtomicU64,
    meta: AtomicU64,
}

/// Ring of the last MMIO accesses plus the device windows they hit.
pub struct MmioTrace {
    started: Instant,
    next: AtomicU64,
    unmapped: AtomicU64,
    dropped: AtomicU64,
    slots: Box<[Slot]>,
    windows: Mutex<Vec<Window>>,
}

impl std::fmt::Debug for MmioTrace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MmioTrace")
            .field("capacity", &self.slots.len())
            .field("total", &self.total())
            .finish()
    }
}

impl Default for MmioTrace {
    fn default() -> Self {
        Self::new(MMIO_TRACE_LEN)
    }
}

impl MmioTrace {
    /// A ring keeping the last `capacity` accesses (at least 1).
    #[tracing::instrument(level = "debug", target = "ternvale::mmio", skip_all, fields(capacity))]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            started: Instant::now(),
            next: AtomicU64::new(0),
            unmapped: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            slots: (0..capacity).map(|_| Slot::default()).collect(),
            windows: Mutex::new(Vec::new()),
        }
    }

    /// Accesses recorded since the VM started, including ones the ring dropped.
    #[tracing::instrument(level = "trace", target = "ternvale::mmio", skip_all)]
    pub fn total(&self) -> u64 {
        self.next.load(Ordering::Acquire)
    }

    /// Accesses that hit no device window.
    #[tracing::instrument(level = "trace", target = "ternvale::mmio", skip_all)]
    pub fn unmapped(&self) -> u64 {
        self.unmapped.load(Ordering::Relaxed)
    }

    /// Events not stored because two vCPUs raced for one ring slot.
    #[tracing::instrument(level = "trace", target = "ternvale::mmio", skip_all)]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Note a device window; returns its index for [`MmioTrace::record`].
    /// `accesses` is the counter the bus increments for this device.
    pub(crate) fn add_window(
        &self,
        name: &str,
        base: u64,
        size: u64,
        accesses: Arc<AtomicU64>,
    ) -> u16 {
        let mut windows = crate::lockwatch::lock(&self.windows, "mmio-trace");
        let index = u16::try_from(windows.len()).unwrap_or(UNMAPPED - 1);
        windows.push(Window {
            name: name.to_string(),
            base,
            size,
            accesses,
        });
        index
    }

    /// Record one access. `device` is from [`MmioTrace::add_window`], or
    /// `None` for an unmapped address.
    pub(crate) fn record(
        &self,
        device: Option<u16>,
        cpu: Option<u64>,
        write: bool,
        gpa: u64,
        size: u8,
        value: u64,
    ) {
        if device.is_none() {
            self.unmapped.fetch_add(1, Ordering::Relaxed);
        }
        let n = self.next.fetch_add(1, Ordering::AcqRel);
        let slot = &self.slots[(n % self.slots.len() as u64) as usize];
        let cpu = cpu.filter(|&id| id < NO_CPU).unwrap_or(NO_CPU);
        let meta = u64::from(size)
            | (u64::from(write) << 8)
            | (u64::from(device.unwrap_or(UNMAPPED)) << 16)
            | (cpu << 32);
        let at_ns = u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let current = slot.seq.load(Ordering::Relaxed);
        let claimed = current % 2 == 0
            && current < 2 * n + 1
            && slot
                .seq
                .compare_exchange(current, 2 * n + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok();
        if !claimed {
            // The ring was lapped: the slot is mid-write or holds a newer event.
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        fence(Ordering::Release);
        slot.at_ns.store(at_ns, Ordering::Relaxed);
        slot.gpa.store(gpa, Ordering::Relaxed);
        slot.value.store(value, Ordering::Relaxed);
        slot.meta.store(meta, Ordering::Relaxed);
        slot.seq.store(2 * n + 2, Ordering::Release);
    }

    /// The accesses still in the ring, oldest first. A slot being rewritten
    /// while this reads it is left out.
    #[tracing::instrument(level = "debug", target = "ternvale::mmio", skip_all)]
    pub fn events(&self) -> Vec<MmioEvent> {
        let end = self.total();
        let start = end.saturating_sub(self.slots.len() as u64);
        let windows = crate::lockwatch::lock(&self.windows, "mmio-trace");
        let mut events = Vec::with_capacity((end - start) as usize);
        for n in start..end {
            let slot = &self.slots[(n % self.slots.len() as u64) as usize];
            let want = 2 * n + 2;
            if slot.seq.load(Ordering::Acquire) != want {
                continue;
            }
            let (at_ns, gpa, value, meta) = (
                slot.at_ns.load(Ordering::Relaxed),
                slot.gpa.load(Ordering::Relaxed),
                slot.value.load(Ordering::Relaxed),
                slot.meta.load(Ordering::Relaxed),
            );
            fence(Ordering::Acquire);
            if slot.seq.load(Ordering::Relaxed) != want {
                continue;
            }
            let index = ((meta >> 16) & 0xffff) as u16;
            let cpu = meta >> 32;
            let window = windows
                .get(usize::from(index))
                .filter(|_| index != UNMAPPED);
            events.push(MmioEvent {
                seq: n,
                at_us: at_ns / 1_000,
                cpu: (cpu != NO_CPU).then_some(cpu as u32),
                write: (meta >> 8) & 1 == 1,
                gpa,
                size: (meta & 0xff) as u8,
                value,
                device: window.map_or_else(|| "unmapped".to_string(), |w| w.name.clone()),
                offset: window.map_or(gpa, |w| gpa.wrapping_sub(w.base)),
            });
        }
        tracing::debug!(target: "ternvale::mmio", total = end, kept = events.len(), "mmio trace read");
        events
    }

    /// Every registered window with its access count, in registration order.
    #[tracing::instrument(level = "debug", target = "ternvale::mmio", skip_all)]
    pub fn devices(&self) -> Vec<DeviceCount> {
        crate::lockwatch::lock(&self.windows, "mmio-trace")
            .iter()
            .map(|window| DeviceCount {
                name: window.name.clone(),
                base: window.base,
                size: window.size,
                accesses: window.accesses.load(Ordering::Relaxed),
            })
            .collect()
    }
}

/// Diagnostics for one VM, shared through [`crate::VmControl::diagnostics`].
#[derive(Debug, Default)]
pub struct Diagnostics {
    dtb: OnceLock<Vec<u8>>,
    mmio: Arc<MmioTrace>,
}

impl Diagnostics {
    /// Empty diagnostics with a [`MMIO_TRACE_LEN`]-event ring.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Keep the DTB handed to the guest. Only the first call is kept.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(bytes = dtb.len()))]
    pub fn set_dtb(&self, dtb: &[u8]) {
        if self.dtb.set(dtb.to_vec()).is_err() {
            tracing::warn!(target: "ternvale::boot", "guest dtb already recorded; keeping the first");
        }
    }

    /// The guest DTB, once the machine has built it.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
    pub fn dtb(&self) -> Option<&[u8]> {
        self.dtb.get().map(Vec::as_slice)
    }

    /// The MMIO ring the machine's bus records into.
    #[tracing::instrument(level = "trace", target = "ternvale::mmio", skip_all)]
    pub fn mmio(&self) -> &Arc<MmioTrace> {
        &self.mmio
    }
}

#[cfg(test)]
#[path = "diag_tests.rs"]
mod tests;
