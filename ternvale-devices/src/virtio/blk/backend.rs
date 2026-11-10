//! Host block storage for virtio-blk (plain raw file or layered COW stack).

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use libc::F_NOCACHE;
use ternvale_config::{DiskCache, DiskFlush};
use ternvale_crypto::{covering_range, DiskXts};
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
    #[allow(dead_code)]
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip_all,
        fields(path = %path.display(), read_only)
    )]
    pub fn open(path: &Path, read_only: bool, options: BlkOptions) -> Result<Self, VirtioBlkError> {
        Self::open_crypto(path, read_only, options, None)
    }

    /// [`open`] with optional AES-256-XTS on disk and COW payloads.
    pub fn open_crypto(
        path: &Path,
        read_only: bool,
        options: BlkOptions,
        xts: Option<DiskXts>,
    ) -> Result<Self, VirtioBlkError> {
        if ternvale_snapshot::is_stack_manifest(path) {
            if options.cache == DiskCache::None {
                tracing::warn!(
                    target: "ternvale::virtio::blk",
                    path = %path.display(),
                    "F_NOCACHE is not applied to layered .tstack images"
                );
            }
            let stack =
                ImageStack::open_crypto(path, read_only, xts).map_err(VirtioBlkError::Snapshot)?;
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
            FileBackend::open_crypto(path, read_only, options, xts).map(Self::from_file)
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

    /// Discard or optionally unmap `[offset, offset+len)`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip_all,
        fields(offset, len, unmap)
    )]
    pub fn discard(&self, offset: u64, len: u64, unmap: bool) -> io::Result<()> {
        match self {
            Self::File(file) => file.discard(offset, len, unmap),
            Self::Stack(stack) => {
                ternvale_vmm::lockwatch::lock(stack, "blk-stack").discard(offset, len, unmap)
            }
        }
    }

    pub fn allocated_bytes(&self) -> io::Result<u64> {
        match self {
            Self::File(file) => file.allocated_bytes(),
            Self::Stack(stack) => ternvale_vmm::lockwatch::lock(stack, "blk-stack")
                .allocated_bytes()
                .map_err(|error| io::Error::other(error.to_string())),
        }
    }

    pub fn in_use_bytes(&self) -> io::Result<u64> {
        match self {
            Self::File(file) => file.in_use_bytes(),
            Self::Stack(stack) => {
                Ok(ternvale_vmm::lockwatch::lock(stack, "blk-stack").in_use_bytes())
            }
        }
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip_all,
        fields(bytes)
    )]
    pub fn resize(&self, bytes: u64) -> io::Result<()> {
        match self {
            Self::File(file) => file.resize(bytes),
            Self::Stack(stack) => ternvale_vmm::lockwatch::lock(stack, "blk-stack")
                .resize(bytes)
                .map_err(|error| io::Error::other(error.to_string())),
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::virtio::blk", skip_all)]
    pub fn compact(&self) -> io::Result<(u64, u64)> {
        match self {
            Self::File(file) => file.compact(),
            Self::Stack(stack) => ternvale_vmm::lockwatch::lock(stack, "blk-stack")
                .compact_layers()
                .map_err(|error| io::Error::other(error.to_string())),
        }
    }
}

/// Plain raw disk image.
pub struct FileBackend {
    file: File,
    bytes: AtomicU64,
    read_only: bool,
    cache: DiskCache,
    flush: DiskFlush,
    sync: Arc<dyn HostSync>,
    xts: Option<DiskXts>,
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
        Self::open_crypto(path, read_only, options, None)
    }

    pub fn open_crypto(
        path: &Path,
        read_only: bool,
        options: BlkOptions,
        xts: Option<DiskXts>,
    ) -> Result<Self, VirtioBlkError> {
        Self::open_with_sync_crypto(path, read_only, options, Arc::new(LibcSync), xts)
    }

    /// Open `path` with an injected [`HostSync`] (tests count `F_FULLFSYNC`).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip_all,
        fields(path = %path.display(), read_only, cache = ?options.cache, flush = ?options.flush)
    )]
    #[allow(dead_code)]
    pub fn open_with_sync(
        path: &Path,
        read_only: bool,
        options: BlkOptions,
        sync: Arc<dyn HostSync>,
    ) -> Result<Self, VirtioBlkError> {
        Self::open_with_sync_crypto(path, read_only, options, sync, None)
    }

    pub fn open_with_sync_crypto(
        path: &Path,
        read_only: bool,
        options: BlkOptions,
        sync: Arc<dyn HostSync>,
        xts: Option<DiskXts>,
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
            encrypted = xts.is_some(),
            "virtio-blk image opened"
        );
        Ok(Self {
            file,
            bytes: AtomicU64::new(bytes),
            read_only,
            cache: options.cache,
            flush: options.flush,
            sync,
            xts,
        })
    }

    fn len(&self) -> u64 {
        self.bytes.load(Ordering::Acquire)
    }

    pub fn capacity(&self) -> u64 {
        self.len() / SECTOR
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    #[allow(dead_code)]
    pub fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        if offset.saturating_add(buf.len() as u64) > self.len() {
            return Err(io::Error::new(ErrorKind::InvalidInput, "read past end"));
        }
        if self.xts.is_none() {
            return read_exact_at(&self.file, buf, offset);
        }
        let (cover_off, cover_len) = covering_range(offset, buf.len());
        let mut cover = vec![0u8; cover_len];
        read_exact_at(&self.file, &mut cover, cover_off)?;
        let xts = self.xts.as_ref().expect("xts");
        xts.crypt(&mut cover, cover_off, false)
            .map_err(|error| io::Error::other(error.to_string()))?;
        let skip = (offset - cover_off) as usize;
        buf.copy_from_slice(&cover[skip..skip + buf.len()]);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.check_write(offset, buf.len() as u64)?;
        if let Some(xts) = &self.xts {
            let (cover_off, cover_len) = covering_range(offset, buf.len());
            let mut cover = vec![0u8; cover_len];
            if cover_off != offset || cover_len != buf.len() {
                read_exact_at(&self.file, &mut cover, cover_off)?;
                xts.crypt(&mut cover, cover_off, false)
                    .map_err(|error| io::Error::other(error.to_string()))?;
            }
            let skip = (offset - cover_off) as usize;
            cover[skip..skip + buf.len()].copy_from_slice(buf);
            xts.crypt(&mut cover, cover_off, true)
                .map_err(|error| io::Error::other(error.to_string()))?;
            write_all_at(&self.file, &cover, cover_off)?;
            return self.after_write();
        }
        write_all_at(&self.file, buf, offset)?;
        self.after_write()
    }

    pub fn read_guest(&self, mem: &GuestMemory, bufs: &[Buffer], offset: u64) -> io::Result<()> {
        let total = bufs.iter().map(|b| u64::from(b.len)).sum::<u64>();
        if offset.saturating_add(total) > self.len() {
            return Err(io::Error::new(ErrorKind::InvalidInput, "read past end"));
        }
        if self.xts.is_some() {
            return self.copy_read_guest(mem, bufs, offset);
        }
        vectored::vectored(&self.file, mem, bufs, offset, true)
    }

    pub fn write_guest(&self, mem: &GuestMemory, bufs: &[Buffer], offset: u64) -> io::Result<()> {
        let total = bufs.iter().map(|b| u64::from(b.len)).sum::<u64>();
        self.check_write(offset, total)?;
        if self.xts.is_some() {
            self.copy_write_guest(mem, bufs, offset)?;
            return self.after_write();
        }
        vectored::vectored(&self.file, mem, bufs, offset, false)?;
        self.after_write()
    }

    fn copy_read_guest(
        &self,
        mem: &GuestMemory,
        bufs: &[Buffer],
        mut offset: u64,
    ) -> io::Result<()> {
        for buf in bufs {
            if buf.len == 0 {
                continue;
            }
            let mut tmp = vec![0u8; buf.len as usize];
            self.read_at(&mut tmp, offset)?;
            let ptr = mem
                .host_ptr(buf.addr, buf.len as usize)
                .map_err(|error| io::Error::new(ErrorKind::InvalidInput, error.to_string()))?;
            // SAFETY: `host_ptr` validated the GPA range.
            unsafe {
                std::ptr::copy_nonoverlapping(tmp.as_ptr(), ptr, tmp.len());
            }
            offset += u64::from(buf.len);
        }
        Ok(())
    }

    fn copy_write_guest(
        &self,
        mem: &GuestMemory,
        bufs: &[Buffer],
        mut offset: u64,
    ) -> io::Result<()> {
        for buf in bufs {
            if buf.len == 0 {
                continue;
            }
            let mut tmp = vec![0u8; buf.len as usize];
            mem.read_bytes(buf.addr, &mut tmp)
                .map_err(|error| io::Error::new(ErrorKind::InvalidInput, error.to_string()))?;
            self.write_at(&tmp, offset)?;
            offset += u64::from(buf.len);
        }
        Ok(())
    }

    pub fn flush(&self) -> io::Result<()> {
        durability_flush(&self.file, self.flush, self.sync.as_ref())
    }

    fn check_write(&self, offset: u64, len: u64) -> io::Result<()> {
        if self.read_only {
            return Err(io::Error::new(ErrorKind::PermissionDenied, "read-only"));
        }
        if offset.saturating_add(len) > self.len() {
            return Err(io::Error::new(ErrorKind::InvalidInput, "write past end"));
        }
        Ok(())
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip_all,
        fields(offset, len, unmap)
    )]
    pub fn discard(&self, offset: u64, len: u64, unmap: bool) -> io::Result<()> {
        self.check_write(offset, len)?;
        if self.xts.is_some() || !unmap {
            return self.write_zeros(offset, len);
        }
        super::punch::punch_hole(&self.file, offset, len)?;
        self.after_write()
    }

    fn write_zeros(&self, offset: u64, len: u64) -> io::Result<()> {
        const CHUNK: usize = 64 * 1024;
        let zeros = vec![0u8; CHUNK];
        let mut done = 0u64;
        while done < len {
            let n = ((len - done) as usize).min(CHUNK);
            self.write_at(&zeros[..n], offset + done)?;
            done += n as u64;
        }
        Ok(())
    }

    pub fn allocated_bytes(&self) -> io::Result<u64> {
        super::punch::allocated_bytes(&self.file)
    }

    pub fn in_use_bytes(&self) -> io::Result<u64> {
        super::punch::in_use_bytes(&self.file, self.len())
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip_all,
        fields(bytes)
    )]
    pub fn resize(&self, bytes: u64) -> io::Result<()> {
        if self.read_only {
            return Err(io::Error::new(ErrorKind::PermissionDenied, "read-only"));
        }
        let aligned = bytes - (bytes % SECTOR);
        self.file.set_len(aligned)?;
        self.bytes.store(aligned, Ordering::Release);
        tracing::info!(
            target: "ternvale::virtio::blk",
            bytes = aligned,
            "raw disk resized"
        );
        Ok(())
    }

    #[tracing::instrument(level = "debug", target = "ternvale::virtio::blk", skip_all)]
    pub fn compact(&self) -> io::Result<(u64, u64)> {
        let n = self.allocated_bytes()?;
        tracing::info!(
            target: "ternvale::virtio::blk",
            before = n,
            after = n,
            "raw disk compact is a no-op (holes already punched)"
        );
        Ok((n, n))
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

fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let n = file.read_at(&mut buf[done..], offset + done as u64)?;
        if n == 0 {
            buf[done..].fill(0);
            return Ok(());
        }
        done += n;
    }
    Ok(())
}

fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let n = file.write_at(&buf[done..], offset + done as u64)?;
        if n == 0 {
            return Err(io::Error::new(ErrorKind::WriteZero, "short pwrite"));
        }
        done += n;
    }
    Ok(())
}

fn durability_flush(file: &File, mode: DiskFlush, sync: &dyn HostSync) -> io::Result<()> {
    match mode {
        DiskFlush::Fast => sync.fsync(file),
        DiskFlush::Safe => sync.full_fsync(file),
    }
}
