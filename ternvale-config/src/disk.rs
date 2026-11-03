//! One `[[disks]]` entry: image path, cache, flush durability, and queueing.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;

/// Default I/O worker threads per virtio-blk device.
pub const DEFAULT_IO_WORKERS: u32 = 2;
/// Default virtqueue size / in-flight request cap.
pub const DEFAULT_QUEUE_DEPTH: u32 = 128;
const MAX_IO_WORKERS: u32 = 16;
const MIN_QUEUE_DEPTH: u32 = 16;
const MAX_QUEUE_DEPTH: u32 = 1024;

/// Host page-cache policy for one disk image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiskCache {
    /// Writes hit the host page cache; guest FLUSH is honored (fastest).
    #[default]
    Writeback,
    /// Every guest write is followed by a host flush.
    Writethrough,
    /// `F_NOCACHE` on the image fd (macOS equivalent of `O_DIRECT`).
    None,
}

/// How a guest virtio-blk FLUSH is issued on macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiskFlush {
    /// `F_FULLFSYNC`: ask the drive to commit. Default.
    #[default]
    Safe,
    /// Plain `fsync`: data reaches the host kernel, not necessarily NAND.
    Fast,
}

/// One virtual disk. `path` must be an existing file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Disk {
    /// Host path of the disk image.
    pub path: PathBuf,
    /// When true, the guest sees a read-only disk.
    pub read_only: bool,
    /// Host cache mode. Default `writeback`.
    #[serde(default, skip_serializing_if = "DiskCache::is_writeback")]
    pub cache: DiskCache,
    /// Guest FLUSH durability. Default `safe`.
    #[serde(default, skip_serializing_if = "DiskFlush::is_safe")]
    pub flush: DiskFlush,
    /// Host I/O threads for this device. Default 2, range `1..=16`.
    #[serde(
        default = "default_io_workers",
        skip_serializing_if = "is_default_workers"
    )]
    pub io_workers: u32,
    /// Virtqueue size and in-flight cap. Default 128, range `16..=1024`.
    #[serde(
        default = "default_queue_depth",
        skip_serializing_if = "is_default_depth"
    )]
    pub queue_depth: u32,
    /// Offer `VIRTIO_BLK_F_MQ` with `num_queues` equal to the vCPU count.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi_queue: bool,
}

impl DiskCache {
    fn is_writeback(&self) -> bool {
        matches!(self, Self::Writeback)
    }
}

impl DiskFlush {
    fn is_safe(&self) -> bool {
        matches!(self, Self::Safe)
    }
}

fn default_io_workers() -> u32 {
    DEFAULT_IO_WORKERS
}

fn default_queue_depth() -> u32 {
    DEFAULT_QUEUE_DEPTH
}

fn is_default_workers(value: &u32) -> bool {
    *value == DEFAULT_IO_WORKERS
}

fn is_default_depth(value: &u32) -> bool {
    *value == DEFAULT_QUEUE_DEPTH
}

impl Default for Disk {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            read_only: false,
            cache: DiskCache::Writeback,
            flush: DiskFlush::Safe,
            io_workers: DEFAULT_IO_WORKERS,
            queue_depth: DEFAULT_QUEUE_DEPTH,
            multi_queue: false,
        }
    }
}

impl Disk {
    /// Range-check worker and queue fields. The image path is checked by
    /// [`crate::VmConfig::validate`].
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::config",
        skip_all,
        fields(index, path = %self.path.display())
    )]
    pub fn validate(&self, index: usize) -> Result<(), ConfigError> {
        if !(1..=MAX_IO_WORKERS).contains(&self.io_workers) {
            tracing::error!(
                target: "ternvale::config",
                index,
                io_workers = self.io_workers,
                "rejected disk io_workers"
            );
            return Err(ConfigError::InvalidDisk {
                field: format!("disks[{index}].io_workers"),
                reason: format!(
                    "must be from 1 to {MAX_IO_WORKERS}, got {}",
                    self.io_workers
                ),
            });
        }
        if !(MIN_QUEUE_DEPTH..=MAX_QUEUE_DEPTH).contains(&self.queue_depth) {
            tracing::error!(
                target: "ternvale::config",
                index,
                queue_depth = self.queue_depth,
                "rejected disk queue_depth"
            );
            return Err(ConfigError::InvalidDisk {
                field: format!("disks[{index}].queue_depth"),
                reason: format!(
                    "must be from {MIN_QUEUE_DEPTH} to {MAX_QUEUE_DEPTH}, got {}",
                    self.queue_depth
                ),
            });
        }
        tracing::debug!(
            target: "ternvale::config",
            index,
            path = %self.path.display(),
            read_only = self.read_only,
            cache = ?self.cache,
            flush = ?self.flush,
            io_workers = self.io_workers,
            queue_depth = self.queue_depth,
            multi_queue = self.multi_queue,
            "accepted disk"
        );
        Ok(())
    }
}
