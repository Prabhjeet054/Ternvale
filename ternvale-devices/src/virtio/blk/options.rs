//! Per-disk I/O policy: cache, flush durability, workers, queues.

use ternvale_config::{DiskCache, DiskFlush, DEFAULT_IO_WORKERS, DEFAULT_QUEUE_DEPTH};

/// How the host file is cached and how guest FLUSH is issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlkOptions {
    /// Host page-cache policy.
    pub cache: DiskCache,
    /// Guest FLUSH / writethrough syscall.
    pub flush: DiskFlush,
    /// Host I/O threads (at least 1).
    pub io_workers: u16,
    /// Virtqueue size and in-flight job cap.
    pub queue_depth: u16,
    /// Number of request virtqueues (`1` unless `VIRTIO_BLK_F_MQ`).
    pub num_queues: u16,
}

impl Default for BlkOptions {
    fn default() -> Self {
        Self {
            cache: DiskCache::Writeback,
            flush: DiskFlush::Safe,
            io_workers: DEFAULT_IO_WORKERS as u16,
            queue_depth: DEFAULT_QUEUE_DEPTH as u16,
            num_queues: 1,
        }
    }
}

impl BlkOptions {
    /// Build from a config block device and the VM's vCPU count.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip_all,
        fields(vcpus, multi_queue = device.multi_queue)
    )]
    pub fn from_device(device: &ternvale_config::BlockDevice, vcpus: u32) -> Self {
        let io_workers = u16::try_from(device.io_workers.max(1)).unwrap_or(1);
        let queue_depth = u16::try_from(device.queue_depth.max(16)).unwrap_or(128);
        let num_queues = if device.multi_queue {
            u16::try_from(vcpus.clamp(1, 16)).unwrap_or(1)
        } else {
            1
        };
        tracing::debug!(
            target: "ternvale::virtio::blk",
            cache = ?device.cache,
            flush = ?device.flush,
            io_workers,
            queue_depth,
            num_queues,
            "blk options"
        );
        Self {
            cache: device.cache,
            flush: device.flush,
            io_workers,
            queue_depth,
            num_queues,
        }
    }
}
