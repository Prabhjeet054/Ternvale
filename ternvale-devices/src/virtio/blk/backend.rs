//! Raw image file for virtio-blk.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::unix::fs::FileExt;
use std::path::Path;

use super::{VirtioBlkError, SECTOR};

/// Host file behind one virtio-blk device.
pub struct FileBackend {
    file: File,
    bytes: u64,
    read_only: bool,
}

impl FileBackend {
    /// Open `path`. Capacity is the file length rounded down to a sector.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::blk",
        skip_all,
        fields(path = %path.display(), read_only)
    )]
    pub fn open(path: &Path, read_only: bool) -> Result<Self, VirtioBlkError> {
        let file = OpenOptions::new()
            .read(true)
            .write(!read_only)
            .open(path)
            .map_err(|source| VirtioBlkError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        let bytes = file
            .metadata()
            .map(|m| m.len())
            .map_err(|source| VirtioBlkError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        let bytes = bytes - (bytes % SECTOR);
        tracing::info!(
            target: "ternvale::virtio::blk",
            path = %path.display(),
            bytes,
            sectors = bytes / SECTOR,
            read_only,
            "virtio-blk image opened"
        );
        Ok(Self {
            file,
            bytes,
            read_only,
        })
    }

    /// Image size in 512-byte sectors.
    pub fn capacity(&self) -> u64 {
        self.bytes / SECTOR
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// `pread` into `buf` at byte `offset`. Sparse holes become zeros.
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

    /// `pwrite` from `buf` at byte `offset`. Does not read-modify-write the file.
    pub fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        if self.read_only {
            return Err(io::Error::new(ErrorKind::PermissionDenied, "read-only"));
        }
        if offset.saturating_add(buf.len() as u64) > self.bytes {
            return Err(io::Error::new(ErrorKind::InvalidInput, "write past end"));
        }
        let mut done = 0;
        while done < buf.len() {
            let n = self.file.write_at(&buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::Error::new(ErrorKind::WriteZero, "short pwrite"));
            }
            done += n;
        }
        Ok(())
    }

    /// Flush cached writes to the host image.
    pub fn flush(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}
