//! Host block storage for virtio-blk (plain raw file or layered COW stack).

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use libc::F_NOCACHE;
use ternvale_config::{DiskCache, DiskFlush};
use ternvale_snapshot::ImageStack;
use ternvale_vmm::GuestMemory;

use super::host_sync::{HostSync, LibcSync};
use super::options::BlkOptions;
use super::vectored;
use super::{Buffer, VirtioBlkError, SECTOR};

#[cfg(test)]
pub(super) use super::host_sync::CountingSync;

/// Host storage behind one virtio-blk device. Shared by I/O worker threads.
#[derive(Clone)]
pub enum BlkBackend {
    /// Single raw image file.
    File(Arc<FileBackend>),
    /// Base image plus ordered delta layers (`.tstack` manifest).
    Stack(Arc<Mutex<ImageStack>>),
}

impl BlkBackend {
    /// Open a raw image or a `.tstack` layered stack at `path`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip_all,
        fields(path = %path.display(), read_only)
    )]
    pub fn open(path: &Path, read_only: bool, options: BlkOptions) -> Result<Self, VirtioBlkError> {
        if ternvale_snapshot::is_stack_manifest(path) {
            if options.cache == DiskCache::None {
                tracing::warn!(
                    target: "ternvale::virtio::blk",
                    path = %path.display(),
                    "F_NOCACHE is not applied to layered .tstack images"
                );
            }
            let stack = ImageStack::open(path, read_only).map_err(VirtioBlkError::Snapshot)?;
            let bytes = stack.virtual_size();
            tracing::info!(
                target = "ternvale::virtio::blk",
                path = %path.display(),
                bytes,
                sectors = bytes / SECTOR,
                layers = "stack",
                read_only,
                cache = ?options.cache,
                flush = ?options.flush,
                "virtio-blk layered image opened"
            );
            Ok(Self::Stack(Arc::new(Mutex::new(stack))))
        } else {
            FileBackend::open(path, read_only, options).map(Self::from_file)
        }
    }

    pub(super) fn from_file(file: FileBackend) -> Self {
        Self::File(Arc::new(file))
    }

    /// Image size in 512-byte sectors.
    pub fn capacity(&self) -> u64 {
        match self {
            Self::File(file) => file.capacity(),
            Self::Stack(stack) => {
                ternvale_vmm::lockwatch::lock(stack, "blk-stack").virtual_size() / SECTOR
            }
        }
    }

    pub fn read_only(&self) -> bool {
        match self {
            Self::File(file) => file.read_only(),
            Self::Stack(stack) => ternvale_vmm::lockwatch::lock(stack, "blk-stack").read_only(),
        }
    }

    /// Read into guest buffers with `preadv` when the backend is a raw file.
    pub fn read_guest(&self, mem: &GuestMemory, bufs: &[Buffer], offset: u64) -> io::Result<()> {
        match self {
            Self::File(file) => file.read_guest(mem, bufs, offset),
            Self::Stack(stack) => vectored::copy_read_guest(
                &ternvale_vmm::lockwatch::lock(stack, "blk-stack"),
                mem,
                bufs,
                offset,
            ),
        }
    }

    /// Write from guest buffers with `pwritev` when the backend is a raw file.
    pub fn write_guest(&self, mem: &GuestMemory, bufs: &[Buffer], offset: u64) -> io::Result<()> {
        match self {
            Self::File(file) => file.write_guest(mem, bufs, offset),
            Self::Stack(stack) => vectored::copy_write_guest(
                &mut ternvale_vmm::lockwatch::lock(stack, "blk-stack"),
                mem,
                bufs,
                offset,
            ),
        }
    }

    pub fn flush(&self) -> io::Result<()> {
        match self {
            Self::File(file) => file.flush(),
            Self::Stack(stack) => ternvale_vmm::lockwatch::lock(stack, "blk-stack").flush(),
        }
    }
}

/// Plain raw disk image.
pub struct FileBackend {
    file: File,
    bytes: u64,
    read_only: bool,
    cache: DiskCache,
    flush: DiskFlush,
    sync: Arc<dyn HostSync>,
}

impl FileBackend {
    /// Open `path`. Capacity is the file length rounded down to a sector.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip_all,
        fields(path = %path.display(), read_only)
    )]
    pub fn open(path: &Path, read_only: bool, options: BlkOptions) -> Result<Self, VirtioBlkError> {
        Self::open_with_sync(path, read_only, options, Arc::new(LibcSync))
    }

    /// Open `path` with an injected [`HostSync`] (tests count `F_FULLFSYNC`).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip_all,
        fields(path = %path.display(), read_only, cache = ?options.cache, flush = ?options.flush)
    )]
    pub fn open_with_sync(
        path: &Path,
        read_only: bool,
        options: BlkOptions,
        sync: Arc<dyn HostSync>,
    ) -> Result<Self, VirtioBlkError> {
        if !read_only {
            reject_shared_base(path)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(!read_only)
            .open(path)
            .map_err(|source| VirtioBlkError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        if options.cache == DiskCache::None {
            set_nocache(&file).map_err(|source| VirtioBlkError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let bytes = file
            .metadata()
            .map(|m| m.len())
            .map_err(|source| VirtioBlkError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        let bytes = bytes - (bytes % SECTOR);
        tracing::info!(
            target = "ternvale::virtio::blk",
            path = %path.display(),
            bytes,
            sectors = bytes / SECTOR,
            read_only,
            cache = ?options.cache,
            flush = ?options.flush,
            "virtio-blk image opened"
        );
        Ok(Self {
            file,
            bytes,
            read_only,
            cache: options.cache,
            flush: options.flush,
            sync,
        })
    }

    pub fn capacity(&self) -> u64 {
        self.bytes / SECTOR
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    #[allow(dead_code)]
    pub fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        if offset.saturating_add(buf.len() as u64) > self.bytes {
            return Err(io::Error::new(ErrorKind::InvalidInput, "read past end"));
        }
        let mut done = 0;
        while done < buf.len() {
            let n = self.file.read_at(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                buf[done..].fill(0);
                return Ok(());
            }
            done += n;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.check_write(offset, buf.len() as u64)?;
        let mut done = 0;
        while done < buf.len() {
            let n = self.file.write_at(&buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::Error::new(ErrorKind::WriteZero, "short pwrite"));
            }
            done += n;
        }
        self.after_write()
    }

    pub fn read_guest(&self, mem: &GuestMemory, bufs: &[Buffer], offset: u64) -> io::Result<()> {
        let total = bufs.iter().map(|b| u64::from(b.len)).sum::<u64>();
        if offset.saturating_add(total) > self.bytes {
            return Err(io::Error::new(ErrorKind::InvalidInput, "read past end"));
        }
        vectored::vectored(&self.file, mem, bufs, offset, true)
    }

    pub fn write_guest(&self, mem: &GuestMemory, bufs: &[Buffer], offset: u64) -> io::Result<()> {
        let total = bufs.iter().map(|b| u64::from(b.len)).sum::<u64>();
        self.check_write(offset, total)?;
        vectored::vectored(&self.file, mem, bufs, offset, false)?;
        self.after_write()
    }

    pub fn flush(&self) -> io::Result<()> {
        durability_flush(&self.file, self.flush, self.sync.as_ref())
    }

    fn check_write(&self, offset: u64, len: u64) -> io::Result<()> {
        if self.read_only {
            return Err(io::Error::new(ErrorKind::PermissionDenied, "read-only"));
        }
        if offset.saturating_add(len) > self.bytes {
            return Err(io::Error::new(ErrorKind::InvalidInput, "write past end"));
        }
        Ok(())
    }

    fn after_write(&self) -> io::Result<()> {
        if self.cache == DiskCache::Writethrough {
            self.flush()
        } else {
            Ok(())
        }
    }
}

fn reject_shared_base(path: &Path) -> Result<(), VirtioBlkError> {
    match ternvale_snapshot::is_write_protected(path) {
        Ok(true) => {
            tracing::warn!(
                target: "ternvale::virtio::blk",
                path = %path.display(),
                "rejected writable open of a shared disk base image"
            );
            Err(VirtioBlkError::Open {
                path: path.to_path_buf(),
                source: io::Error::new(
                    ErrorKind::PermissionDenied,
                    "disk image is a shared clone base and cannot be opened for write",
                ),
            })
        }
        Ok(false) => Ok(()),
        Err(error) => {
            tracing::warn!(
                target: "ternvale::virtio::blk",
                path = %path.display(),
                error = %error,
                "could not check shared-base protection; opening anyway"
            );
            Ok(())
        }
    }
}

fn set_nocache(file: &File) -> io::Result<()> {
    // SAFETY: `file` is an open fd; F_NOCACHE takes an int argument.
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), F_NOCACHE, 1) };
    if rc == -1 {
        Err(io::Error::last_os_error())
    } else {
        tracing::debug!(target: "ternvale::virtio::blk", "set F_NOCACHE on disk image");
        Ok(())
    }
}

fn durability_flush(file: &File, mode: DiskFlush, sync: &dyn HostSync) -> io::Result<()> {
    match mode {
        DiskFlush::Fast => sync.fsync(file),
        DiskFlush::Safe => sync.full_fsync(file),
    }
}
