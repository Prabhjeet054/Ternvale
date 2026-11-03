//! Host durability syscalls behind virtio-blk FLUSH / writethrough.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU64, Ordering};

/// Durability operations on an open image fd.
pub trait HostSync: Send + Sync {
    /// `fcntl(F_FULLFSYNC)` (macOS platter/NAND flush).
    fn full_fsync(&self, file: &File) -> io::Result<()>;
    /// `fsync` / `File::sync_all`.
    fn fsync(&self, file: &File) -> io::Result<()>;
}

/// Real Darwin `fcntl` / `fsync` implementation.
pub struct LibcSync;

impl HostSync for LibcSync {
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::blk", skip_all)]
    fn full_fsync(&self, file: &File) -> io::Result<()> {
        // SAFETY: `file` is an open fd; `F_FULLFSYNC` takes no extra argument.
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) };
        if rc == -1 {
            let error = io::Error::last_os_error();
            tracing::warn!(
                target: "ternvale::virtio::blk",
                error = %error,
                "F_FULLFSYNC failed; falling back to fsync"
            );
            file.sync_all()
        } else {
            tracing::debug!(target: "ternvale::virtio::blk", "F_FULLFSYNC");
            Ok(())
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::virtio::blk", skip_all)]
    fn fsync(&self, file: &File) -> io::Result<()> {
        tracing::debug!(target: "ternvale::virtio::blk", "fsync");
        file.sync_all()
    }
}

/// Counts durability calls; used by unit tests instead of real `fcntl`.
#[derive(Default)]
pub struct CountingSync {
    /// `F_FULLFSYNC` invocations.
    pub full_fsync: AtomicU64,
    /// `fsync` invocations.
    pub fsync: AtomicU64,
}

impl HostSync for CountingSync {
    fn full_fsync(&self, _file: &File) -> io::Result<()> {
        self.full_fsync.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn fsync(&self, _file: &File) -> io::Result<()> {
        self.fsync.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
