//! Failures when opening a share or attaching the device.

use std::path::PathBuf;

/// Opening the export or attaching virtio-fs failed.
#[derive(Debug, thiserror::Error)]
pub enum FsError {
    /// MMIO slot out of range.
    #[error("virtio-fs slot {slot} is outside the virtio-mmio window")]
    BadSlot {
        /// Requested slot.
        slot: u32,
    },
    /// Host export path is not a usable directory.
    #[error("virtio-fs export {}: {source}", path.display())]
    Export {
        /// Host path.
        path: PathBuf,
        /// OS error.
        source: std::io::Error,
    },
    /// Worker thread failed to start.
    #[error("virtio-fs worker: {0}")]
    Worker(std::io::Error),
}

/// Map `std::io::Error` to a FUSE errno (positive).
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub fn io_errno(error: &std::io::Error) -> i32 {
    error.raw_os_error().unwrap_or(libc::EIO)
}
