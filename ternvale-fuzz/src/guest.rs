//! Fake guest RAM and a split-virtqueue layout a guest driver would program.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_devices::VirtioIrq;
use ternvale_vmm::{GuestMemory, HOST_PAGE_SIZE};

/// GPA of the main scratch region (descriptor table at the start).
pub const RAM_GPA: u64 = 0x4000_0000;
/// Queue size used by harnesses (power of two).
pub const QUEUE_SIZE: u16 = 8;
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

/// Guest memory plus a virtio IRQ line.
pub struct Guest {
    /// Shared with device workers.
    pub memory: Arc<Mutex<GuestMemory>>,
    /// Shared with the virtio transport.
    pub irq: Arc<VirtioIrq>,
    /// [`RAM_GPA`].
    pub base: u64,
}

impl Guest {
    /// Anonymous RAM at GPA 0 (one page) and [`RAM_GPA`] (four pages).
    #[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all)]
    pub fn new() -> Option<Self> {
        let mut mem = GuestMemory::new().ok()?;
        mem.add_region(0, HOST_PAGE_SIZE).ok()?;
        mem.add_region(RAM_GPA, HOST_PAGE_SIZE * 4).ok()?;
        Some(Self {
            memory: Arc::new(Mutex::new(mem)),
            irq: VirtioIrq::new(),
            base: RAM_GPA,
        })
    }

    /// Descriptor table GPA.
    #[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all)]
    pub fn desc(&self) -> u64 {
        self.base
    }

    /// Available ring GPA.
    #[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all)]
    pub fn avail(&self) -> u64 {
        self.base + 0x400
    }

    /// Used ring GPA.
    #[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all)]
    pub fn used(&self) -> u64 {
        self.base + 0x800
    }

    /// Payload GPA (after the rings).
    #[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all)]
    pub fn payload(&self) -> u64 {
        self.base + 0x1000
    }

    /// Write `bytes` at `gpa`. Out-of-range writes are dropped.
    #[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(gpa = format!("{gpa:#x}"), len = bytes.len()))]
    pub fn write(&self, gpa: u64, bytes: &[u8]) {
        if let Ok(mut mem) = self.memory.lock() {
            if let Err(error) = mem.write_bytes(gpa, bytes) {
                tracing::trace!(
                    target: "ternvale::fuzz",
                    gpa = format!("{gpa:#x}"),
                    error = %error,
                    "guest write rejected"
                );
            }
        }
    }

    /// One 16-byte descriptor.
    #[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all)]
    pub fn write_desc(&self, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let at = self.base + u64::from(index) * 16;
        if let Ok(mut mem) = self.memory.lock() {
            note_write(mem.write_u64(at, addr).map(|_| ()), at);
            note_write(mem.write_u32(at + 8, len).map(|_| ()), at + 8);
            note_write(mem.write_u16(at + 12, flags).map(|_| ()), at + 12);
            note_write(mem.write_u16(at + 14, next).map(|_| ()), at + 14);
        }
    }

    /// Publish `heads` in the available ring and bump `idx`.
    #[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all)]
    pub fn publish(&self, heads: &[u16]) {
        if let Ok(mut mem) = self.memory.lock() {
            for (slot, head) in heads.iter().enumerate() {
                let at = self.avail() + 4 + (slot as u64) * 2;
                note_write(mem.write_u16(at, *head).map(|_| ()), at);
            }
            note_write(
                mem.write_u16(self.avail() + 2, heads.len() as u16)
                    .map(|_| ()),
                self.avail() + 2,
            );
        }
    }

    /// Fill payload from `data` and build a short descriptor chain.
    ///
    /// Byte 0 of `data` selects writable vs readable for the first buffer.
    #[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
    pub fn load_chain(&self, data: &[u8]) {
        let payload = self.payload();
        let body = if data.len() > 1 { &data[1..] } else { data };
        let cap = 1024.min(body.len());
        self.write(payload, &body[..cap]);
        let writable = data.first().copied().unwrap_or(0) & 1 != 0;
        let flags = if writable { DESC_WRITE } else { 0 };
        let len = if cap == 0 { 8 } else { cap as u32 };
        self.write_desc(0, payload, len, flags, 0);
        if data.first().copied().unwrap_or(0) & 2 != 0 {
            self.write_desc(0, payload, len / 2 + 1, flags | DESC_NEXT, 1);
            self.write_desc(1, payload + 64, 64, DESC_WRITE, 0);
        }
        self.publish(&[0]);
    }

    /// Poll the used ring until `idx >= min` or `budget` elapses.
    #[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all)]
    pub fn wait_used(&self, min: u16, budget: Duration) {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if let Ok(mem) = self.memory.lock() {
                if mem
                    .read_u16(self.used() + 2)
                    .ok()
                    .is_some_and(|idx| idx >= min)
                {
                    return;
                }
            }
            std::thread::yield_now();
        }
    }
}

/// Remove a scratch directory; failures are logged, not swallowed silently.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all)]
pub fn cleanup(path: &std::path::Path) {
    if let Err(error) = std::fs::remove_dir_all(path) {
        tracing::debug!(
            target: "ternvale::fuzz",
            path = %path.display(),
            error = %error,
            "scratch cleanup"
        );
    }
}

/// Unique path under the process temp directory.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(kind))]
pub fn scratch(kind: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "ternvale-fuzz-{}-{}-{}",
        kind,
        std::process::id(),
        n
    ))
}

fn note_write(result: Result<(), ternvale_vmm::MemoryError>, gpa: u64) {
    if let Err(error) = result {
        tracing::trace!(
            target: "ternvale::fuzz",
            gpa = format!("{gpa:#x}"),
            error = %error,
            "guest write rejected"
        );
    }
}
