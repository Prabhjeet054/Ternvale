//! virtio-blk device: request queues, file backend, and I/O workers.

mod backend;
mod handle;
mod host_sync;
mod media;
mod options;
mod pool;
mod punch;
mod request;
mod vectored;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ternvale_metrics::Metrics;
use ternvale_vmm::pci::Bdf;
use ternvale_vmm::{DeviceAttach, GuestMemory, MachineError};

use super::irq::{IrqHook, VirtioIrq};
use super::{QueueNotify, VirtioDevice, VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC};
use backend::BlkBackend;
pub use handle::DiskHandle;
pub use media::{
    BlkMedia, OPTICAL_BLOCK_SIZE, VIRTIO_BLK_F_BLK_SIZE, VIRTIO_BLK_F_MQ, VIRTIO_BLK_F_SEG_MAX,
};
pub use options::BlkOptions;
use pool::{Kick, Pool};

/// Virtio block device id.
pub const VIRTIO_BLK_ID: u32 = 2;
/// Read-only feature bit.
pub const VIRTIO_BLK_F_RO: u64 = 1 << 5;
/// Cache flush feature bit.
pub const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;
/// Discard (TRIM) feature bit (virtio 1.2 §5.2.3).
pub const VIRTIO_BLK_F_DISCARD: u64 = 1 << 13;
/// Write zeroes feature bit (virtio 1.2 §5.2.3).
pub const VIRTIO_BLK_F_WRITE_ZEROES: u64 = 1 << 14;

pub(super) const VIRTIO_BLK_T_IN: u32 = 0;
pub(super) const VIRTIO_BLK_T_OUT: u32 = 1;
pub(super) const VIRTIO_BLK_T_FLUSH: u32 = 4;
pub(super) const VIRTIO_BLK_T_GET_ID: u32 = 8;
pub(super) const VIRTIO_BLK_T_DISCARD: u32 = 11;
pub(super) const VIRTIO_BLK_T_WRITE_ZEROES: u32 = 13;
pub(super) const VIRTIO_BLK_DISCARD_UNMAP: u32 = 1;
pub(super) const VIRTIO_BLK_S_OK: u8 = 0;
pub(super) const VIRTIO_BLK_S_IOERR: u8 = 1;
pub(super) const VIRTIO_BLK_S_UNSUPP: u8 = 2;
pub(super) const SECTOR: u64 = 512;
pub(super) const ID_BYTES: usize = 20;
pub(super) const INT_VRING: u32 = 1;
pub(super) const INT_CONFIG: u32 = 2;

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
    /// Layered snapshot stack error.
    #[error("virtio-blk snapshot: {0}")]
    Snapshot(ternvale_snapshot::SnapshotError),
    /// AES-256-XTS failed (unaligned I/O or bad key).
    #[error("virtio-blk encryption: {0}")]
    Crypto(ternvale_crypto::CryptoError),
}

/// Base, size, MMIO device, and live stats from [`VirtioBlk::attach`].
pub type AttachedBlk = (
    u64,
    u64,
    Box<dyn ternvale_vmm::MmioDevice>,
    Arc<BlkStats>,
    Arc<DiskHandle>,
);

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

#[inline]
fn notify_mark() -> Option<std::time::Instant> {
    #[cfg(feature = "metrics")]
    {
        Some(std::time::Instant::now())
    }
    #[cfg(not(feature = "metrics"))]
    {
        None
    }
}

/// Whether `path` may be opened writable on the host (shared clone bases are rejected).
#[doc(hidden)]
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtio::blk",
    skip_all,
    fields(path = %path.display())
)]
pub fn try_open_disk_for_write(path: &Path) -> Result<(), VirtioBlkError> {
    backend::FileBackend::open(path, false, BlkOptions::default()).map(|_| ())
}

/// One virtio-blk device with a pool of I/O threads.
pub struct VirtioBlk {
    media: BlkMedia,
    options: BlkOptions,
    handle: Arc<DiskHandle>,
    stats: Arc<BlkStats>,
    pool: Option<Pool>,
}

impl VirtioBlk {
    /// Open `path` and start I/O workers.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip(memory, irq, metrics),
        fields(path = %path.display(), read_only)
    )]
    pub fn open(
        path: &Path,
        read_only: bool,
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
        metrics: Arc<Metrics>,
    ) -> Result<Self, VirtioBlkError> {
        Self::open_media(
            path,
            BlkMedia::Disk { read_only },
            BlkOptions::default(),
            memory,
            irq,
            metrics,
        )
    }

    /// Open `path` as `media` with `options` and start I/O workers.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip(memory, irq, metrics),
        fields(path = %path.display(), media = media.name())
    )]
    pub fn open_media(
        path: &Path,
        media: BlkMedia,
        options: BlkOptions,
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
        metrics: Arc<Metrics>,
    ) -> Result<Self, VirtioBlkError> {
        Self::open_media_crypto(path, media, options, memory, irq, metrics, None)
    }

    /// [`open_media`] with AES-256-XTS on disk images (not optical ISOs).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip(memory, irq, metrics, xts),
        fields(path = %path.display(), media = media.name(), encrypted = xts.is_some())
    )]
    pub fn open_media_crypto(
        path: &Path,
        media: BlkMedia,
        options: BlkOptions,
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
        metrics: Arc<Metrics>,
        xts: Option<ternvale_crypto::DiskXts>,
    ) -> Result<Self, VirtioBlkError> {
        let xts = if matches!(media, BlkMedia::Optical) {
            None
        } else {
            xts
        };
        let backend = BlkBackend::open_crypto(path, media.read_only(), options, xts)?;
        let capacity = backend.capacity();
        tracing::info!(
            target: "ternvale::virtio::blk",
            path = %path.display(),
            media = media.name(),
            capacity_sectors = capacity,
            features = format!("{:#x}", Self::feature_bits(media, &options)),
            io_workers = options.io_workers,
            queue_depth = options.queue_depth,
            num_queues = options.num_queues,
            "virtio-blk medium"
        );
        let mut serial = [0u8; ID_BYTES];
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("ternvale");
        let bytes = name.as_bytes();
        let n = bytes.len().min(ID_BYTES);
        serial[..n].copy_from_slice(&bytes[..n]);
        let stats = Arc::new(BlkStats::default());
        let disk_id = metrics.register_blk_disk(name);
        let handle = DiskHandle::new(backend.clone(), media, options.num_queues, Arc::clone(&irq));
        let pool = Pool::spawn(
            options,
            backend,
            memory,
            irq,
            Arc::clone(&stats),
            serial,
            metrics,
            disk_id,
        )
        .map_err(|source| VirtioBlkError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self {
            media,
            options,
            handle,
            stats,
            pool: Some(pool),
        })
    }

    fn feature_bits(media: BlkMedia, options: &BlkOptions) -> u64 {
        let mut bits =
            media.features() | VIRTIO_F_EVENT_IDX | VIRTIO_F_INDIRECT_DESC | VIRTIO_BLK_F_SEG_MAX;
        if options.num_queues > 1 {
            bits |= VIRTIO_BLK_F_MQ;
        }
        bits
    }

    /// Build a transport on `slot` for this open image.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip(memory, irq_hook, metrics),
        fields(slot, path = %path.display(), read_only)
    )]
    pub fn attach(
        slot: u32,
        path: &Path,
        read_only: bool,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
        metrics: Arc<Metrics>,
    ) -> Result<AttachedBlk, VirtioBlkError> {
        Self::attach_media(
            slot,
            path,
            BlkMedia::Disk { read_only },
            BlkOptions::default(),
            memory,
            irq_hook,
            metrics,
        )
    }

    /// Build a virtio-mmio transport on `slot` for `path` opened as `media`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip(memory, irq_hook, metrics),
        fields(slot, path = %path.display(), media = media.name())
    )]
    pub fn attach_media(
        slot: u32,
        path: &Path,
        media: BlkMedia,
        options: BlkOptions,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
        metrics: Arc<Metrics>,
    ) -> Result<AttachedBlk, VirtioBlkError> {
        Self::attach_media_crypto(slot, path, media, options, memory, irq_hook, metrics, None)
    }

    /// [`attach_media`] with AES-256-XTS.
    #[allow(clippy::too_many_arguments)]
    pub fn attach_media_crypto(
        slot: u32,
        path: &Path,
        media: BlkMedia,
        options: BlkOptions,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
        metrics: Arc<Metrics>,
        xts: Option<ternvale_crypto::DiskXts>,
    ) -> Result<AttachedBlk, VirtioBlkError> {
        let irq = VirtioIrq::new();
        irq.set_hook(irq_hook);
        let blk =
            Self::open_media_crypto(path, media, options, memory, Arc::clone(&irq), metrics, xts)?;
        let stats = Arc::clone(&blk.stats);
        let handle = Arc::clone(&blk.handle);
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
            handle,
        ))
    }

    /// Open `path` as a virtio-pci function on the next free device of bus 0.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip(attach),
        fields(path = %path.display(), read_only)
    )]
    pub fn attach_pci(
        attach: &DeviceAttach,
        path: &Path,
        read_only: bool,
    ) -> Result<(Bdf, Arc<BlkStats>, Arc<DiskHandle>), MachineError> {
        Self::attach_pci_media(
            attach,
            path,
            BlkMedia::Disk { read_only },
            BlkOptions::default(),
        )
    }

    /// Open `path` as `media` on the next free virtio-pci function of bus 0.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip(attach),
        fields(path = %path.display(), media = media.name())
    )]
    pub fn attach_pci_media(
        attach: &DeviceAttach,
        path: &Path,
        media: BlkMedia,
        options: BlkOptions,
    ) -> Result<(Bdf, Arc<BlkStats>, Arc<DiskHandle>), MachineError> {
        Self::attach_pci_media_crypto(attach, path, media, options, None)
    }

    /// [`attach_pci_media`] with AES-256-XTS.
    pub fn attach_pci_media_crypto(
        attach: &DeviceAttach,
        path: &Path,
        media: BlkMedia,
        options: BlkOptions,
        xts: Option<ternvale_crypto::DiskXts>,
    ) -> Result<(Bdf, Arc<BlkStats>, Arc<DiskHandle>), MachineError> {
        let irq = VirtioIrq::new();
        let blk = Self::open_media_crypto(
            path,
            media,
            options,
            Arc::clone(&attach.memory),
            Arc::clone(&irq),
            Arc::clone(&attach.metrics),
            xts,
        )
        .map_err(|error| MachineError::Attach(error.to_string()))?;
        let stats = Arc::clone(&blk.stats);
        let handle = Arc::clone(&blk.handle);
        let bdf = super::pci::attach_pci(attach, Box::new(blk), irq)?;
        Ok((bdf, stats, handle))
    }

    /// Per-device request counters.
    pub fn stats(&self) -> &BlkStats {
        &self.stats
    }

    /// Online resize / config-change handle.
    pub fn disk_handle(&self) -> Arc<DiskHandle> {
        Arc::clone(&self.handle)
    }

    fn send(&self, kick: Kick) {
        let Some(pool) = self.pool.as_ref() else {
            return;
        };
        let Some(tx) = pool.kick.as_ref() else {
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
        self.pool.take();
    }
}

impl VirtioDevice for VirtioBlk {
    fn device_id(&self) -> u32 {
        VIRTIO_BLK_ID
    }

    fn device_features(&self) -> u64 {
        Self::feature_bits(self.media, &self.options)
    }

    fn num_queues(&self) -> u16 {
        self.options.num_queues.max(1)
    }

    fn queue_num_max(&self, _queue: u16) -> u16 {
        self.options.queue_depth.max(16)
    }

    fn read_config(&self, offset: u64, size: u8) -> u64 {
        self.handle.read_config(offset, size)
    }

    fn config_generation(&self) -> u32 {
        self.handle.generation()
    }

    fn notify(&self, queue: QueueNotify) {
        self.send(Kick::Notify(queue, notify_mark()));
    }

    fn status_changed(&self, status: u32) {
        if status == 0 {
            self.send(Kick::Reset);
        }
    }
}

#[cfg(test)]
#[path = "blk_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "blk_cache_tests.rs"]
mod cache_tests;

#[cfg(test)]
#[path = "blk_crypto_tests.rs"]
mod crypto_tests;
