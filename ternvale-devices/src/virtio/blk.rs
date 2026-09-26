//! virtio-blk device: request queue, file backend, and I/O thread.

mod backend;
mod request;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ternvale_vmm::GuestMemory;

use super::irq::{IrqHook, VirtioIrq};
use super::queue::SplitQueue;
use super::{QueueNotify, VirtioDevice, VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC};
use backend::FileBackend;

/// Virtio block device id.
pub const VIRTIO_BLK_ID: u32 = 2;
/// Read-only feature bit.
pub const VIRTIO_BLK_F_RO: u64 = 1 << 5;
/// Cache flush feature bit.
pub const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;

pub(super) const VIRTIO_BLK_T_IN: u32 = 0;
pub(super) const VIRTIO_BLK_T_OUT: u32 = 1;
pub(super) const VIRTIO_BLK_T_FLUSH: u32 = 4;
pub(super) const VIRTIO_BLK_T_GET_ID: u32 = 8;
pub(super) const VIRTIO_BLK_S_OK: u8 = 0;
pub(super) const VIRTIO_BLK_S_IOERR: u8 = 1;
pub(super) const VIRTIO_BLK_S_UNSUPP: u8 = 2;
pub(super) const SECTOR: u64 = 512;
pub(super) const ID_BYTES: usize = 20;

const INT_VRING: u32 = 1;

pub(super) use super::queue::Buffer;

/// Opening or sizing a virtio-blk image failed.
#[derive(Debug, thiserror::Error)]
pub enum VirtioBlkError {
    /// The image file could not be opened.
    #[error("open virtio-blk image {}: {source}", path.display())]
    Open {
        /// Host path.
        path: PathBuf,
        /// OS error.
        source: std::io::Error,
    },
}

/// Base, size, MMIO device, and live stats from [`VirtioBlk::attach`].
pub type AttachedBlk = (u64, u64, Box<dyn ternvale_vmm::MmioDevice>, Arc<BlkStats>);

/// Counters for one virtio-blk device.
#[derive(Debug, Default)]
pub struct BlkStats {
    /// Completed requests, including failures.
    pub reqs: AtomicU64,
    /// Payload bytes read or written on successful IN/OUT/GET_ID.
    pub bytes: AtomicU64,
    /// Requests that returned an error status.
    pub errors: AtomicU64,
}

impl BlkStats {
    pub(super) fn ok(&self, bytes: u32) {
        self.reqs.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(u64::from(bytes), Ordering::Relaxed);
    }

    pub(super) fn error(&self) {
        self.reqs.fetch_add(1, Ordering::Relaxed);
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot of `(reqs, bytes, errors)`.
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.reqs.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed),
        )
    }
}

enum Kick {
    Notify(QueueNotify),
    Reset,
}

/// One virtio-blk device with a dedicated I/O thread.
pub struct VirtioBlk {
    capacity: u64,
    read_only: bool,
    stats: Arc<BlkStats>,
    kick: Option<Sender<Kick>>,
    join: Option<JoinHandle<()>>,
}

impl VirtioBlk {
    /// Open `path` and start the I/O thread.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip(memory, irq),
        fields(path = %path.display(), read_only)
    )]
    pub fn open(
        path: &Path,
        read_only: bool,
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
    ) -> Result<Self, VirtioBlkError> {
        let backend = FileBackend::open(path, read_only)?;
        let capacity = backend.capacity();
        let mut serial = [0u8; ID_BYTES];
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("ternvale");
        let bytes = name.as_bytes();
        let n = bytes.len().min(ID_BYTES);
        serial[..n].copy_from_slice(&bytes[..n]);
        let stats = Arc::new(BlkStats::default());
        let (tx, rx) = mpsc::channel();
        let worker_stats = Arc::clone(&stats);
        let join = std::thread::Builder::new()
            .name("ternvale-virtio-blk".into())
            .spawn(move || worker(rx, backend, memory, irq, worker_stats, serial))
            .map_err(|source| VirtioBlkError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            capacity,
            read_only,
            stats,
            kick: Some(tx),
            join: Some(join),
        })
    }

    /// Build a transport on `slot` for this open image.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip(memory, irq_hook),
        fields(slot, path = %path.display(), read_only)
    )]
    pub fn attach(
        slot: u32,
        path: &Path,
        read_only: bool,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
    ) -> Result<AttachedBlk, VirtioBlkError> {
        let irq = VirtioIrq::new();
        irq.set_hook(irq_hook);
        let blk = Self::open(path, read_only, memory, Arc::clone(&irq))?;
        let stats = Arc::clone(&blk.stats);
        let base = crate::virtio::slot_base(slot).ok_or_else(|| VirtioBlkError::Open {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad virtio slot"),
        })?;
        let transport = super::mmio::VirtioMmio::with_irq(slot, Box::new(blk), irq);
        Ok((
            base,
            ternvale_vmm::VIRTIO_MMIO_SLOT_SIZE,
            Box::new(transport),
            stats,
        ))
    }

    /// Per-device request counters.
    pub fn stats(&self) -> &BlkStats {
        &self.stats
    }

    fn send(&self, kick: Kick) {
        let Some(tx) = self.kick.as_ref() else {
            return;
        };
        if let Err(error) = tx.send(kick) {
            tracing::error!(
                target: "ternvale::virtio::blk",
                error = %error,
                "virtio-blk kick failed"
            );
        }
    }
}

impl Drop for VirtioBlk {
    fn drop(&mut self) {
        self.kick.take();
        if let Some(join) = self.join.take() {
            if let Err(error) = join.join() {
                tracing::error!(
                    target: "ternvale::virtio::blk",
                    ?error,
                    "virtio-blk worker panicked"
                );
            }
        }
    }
}

impl VirtioDevice for VirtioBlk {
    fn device_id(&self) -> u32 {
        VIRTIO_BLK_ID
    }

    fn device_features(&self) -> u64 {
        let mut features = VIRTIO_BLK_F_FLUSH;
        if self.read_only {
            features |= VIRTIO_BLK_F_RO;
        }
        features
    }

    fn num_queues(&self) -> u16 {
        1
    }

    fn read_config(&mut self, offset: u64, size: u8) -> u64 {
        let bytes = self.capacity.to_le_bytes();
        read_le(&bytes, offset, size)
    }

    fn notify(&mut self, queue: QueueNotify) {
        self.send(Kick::Notify(queue));
    }

    fn status_changed(&mut self, status: u32) {
        if status == 0 {
            self.send(Kick::Reset);
        }
    }
}

fn worker(
    rx: Receiver<Kick>,
    backend: FileBackend,
    memory: Arc<Mutex<GuestMemory>>,
    irq: Arc<VirtioIrq>,
    stats: Arc<BlkStats>,
    serial: [u8; ID_BYTES],
) {
    let mut queue: Option<SplitQueue> = None;
    let mut rings: Option<(u16, u64, u64, u64)> = None;
    while let Ok(kick) = rx.recv() {
        match kick {
            Kick::Reset => {
                queue = None;
                rings = None;
                tracing::debug!(target: "ternvale::virtio::blk", "virtio-blk queues reset");
            }
            Kick::Notify(notify) => {
                let key = (notify.size, notify.desc, notify.avail, notify.used);
                if rings != Some(key) {
                    let indirect = notify.features & VIRTIO_F_INDIRECT_DESC != 0;
                    let event_idx = notify.features & VIRTIO_F_EVENT_IDX != 0;
                    queue = Some(SplitQueue::new(
                        notify.size,
                        notify.desc,
                        notify.avail,
                        notify.used,
                        indirect,
                        event_idx,
                    ));
                    rings = Some(key);
                }
                let Some(q) = queue.as_mut() else {
                    continue;
                };
                let mut mem = match memory.lock() {
                    Ok(guard) => guard,
                    Err(poison) => poison.into_inner(),
                };
                let mut completed = false;
                let chains: Vec<_> = q.chains(&mem).collect();
                for chain in chains {
                    let used = request::process(&mut mem, &backend, &chain, &serial, &stats);
                    if q.add_used(&mut mem, chain.head, used) {
                        completed = true;
                    }
                }
                if completed && q.notification_needed(&mem) {
                    irq.raise(INT_VRING);
                }
            }
        }
    }
}

fn read_le(bytes: &[u8], offset: u64, size: u8) -> u64 {
    let start = offset as usize;
    if start >= bytes.len() {
        return 0;
    }
    let mut tmp = [0u8; 8];
    let n = (size as usize).min(8).min(bytes.len() - start);
    tmp[..n].copy_from_slice(&bytes[start..start + n]);
    u64::from_le_bytes(tmp)
}

#[cfg(test)]
#[path = "blk_tests.rs"]
mod tests;
