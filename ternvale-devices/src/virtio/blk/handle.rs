//! Live virtio-blk control: online grow and config-change interrupt.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use super::backend::BlkBackend;
use super::media::BlkMedia;
use super::{INT_CONFIG, SECTOR};
use crate::virtio::irq::VirtioIrq;

/// Shared by the MMIO device and the control socket.
pub struct DiskHandle {
    pub(super) backend: BlkBackend,
    pub(super) config: Mutex<[u8; super::media::CONFIG_BYTES]>,
    pub(super) generation: AtomicU32,
    pub(super) irq: Arc<VirtioIrq>,
    media: BlkMedia,
    num_queues: u16,
}

impl DiskHandle {
    pub(super) fn new(
        backend: BlkBackend,
        media: BlkMedia,
        num_queues: u16,
        irq: Arc<VirtioIrq>,
    ) -> Arc<Self> {
        let capacity = backend.capacity();
        Arc::new(Self {
            backend,
            config: Mutex::new(media.config_space(capacity, num_queues)),
            generation: AtomicU32::new(0),
            irq,
            media,
            num_queues,
        })
    }

    /// Grow (or forced-shrink) the image and notify the guest.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip_all,
        fields(bytes, force)
    )]
    pub fn resize(&self, bytes: u64, force: bool) -> Result<(u64, u64, u64), std::io::Error> {
        if self.media.read_only() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "read-only disk",
            ));
        }
        let before = self.backend.allocated_bytes()?;
        let in_use = self.backend.in_use_bytes()?;
        let aligned = bytes - (bytes % SECTOR);
        if aligned < in_use && !force {
            tracing::warn!(
                target: "ternvale::virtio::blk",
                aligned,
                in_use,
                "refusing shrink below in-use size"
            );
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing to shrink below in-use size {in_use} bytes (use --force)"),
            ));
        }
        self.backend.resize(aligned)?;
        let capacity = aligned / SECTOR;
        {
            let mut cfg = ternvale_vmm::lockwatch::lock(&self.config, "blk-config");
            *cfg = self.media.config_space(capacity, self.num_queues);
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.irq.raise(INT_CONFIG);
        let after = self.backend.allocated_bytes()?;
        tracing::info!(
            target: "ternvale::virtio::blk",
            before,
            after,
            logical = aligned,
            "disk resized"
        );
        Ok((before, after, aligned))
    }

    /// Rewrite delta layers, dropping unmapped packed blocks.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::blk", skip_all)]
    pub fn compact(&self) -> Result<(u64, u64), std::io::Error> {
        if self.media.read_only() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "read-only disk",
            ));
        }
        self.backend.compact()
    }

    pub(super) fn read_config(&self, offset: u64, size: u8) -> u64 {
        let cfg = ternvale_vmm::lockwatch::lock(&self.config, "blk-config");
        super::media::read_le(&cfg[..], offset, size)
    }

    pub(super) fn generation(&self) -> u32 {
        self.generation.load(Ordering::Acquire)
    }
}
