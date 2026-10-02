//! The per-VM NVRAM file behind the variable-store flash bank.
//!
//! The file holds the first `len` bytes of the bank; anything after it reads
//! as zero, like a zero-padded `QEMU_VARS.fd`. A missing file is created
//! empty: EDK2 then sees no valid firmware-volume header, erases the store,
//! and writes fresh headers (`VirtNorFlashDxe.c`). Each program or erase is
//! written through with `pwrite`; the file is synced when the bank is dropped.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use super::FirmwareError;

/// An open NVRAM file.
#[derive(Debug)]
pub(super) struct NvramFile {
    file: File,
    path: PathBuf,
    dirty: bool,
}

impl NvramFile {
    /// Open or create `path` and copy its contents into `bank`.
    #[tracing::instrument(level = "debug", target = "ternvale::firmware", skip_all, fields(path = %path.display()))]
    pub(super) fn open(path: &Path, bank: &mut [u8]) -> Result<Self, FirmwareError> {
        let fail = |what: &'static str| {
            let path = path.to_path_buf();
            move |source: std::io::Error| {
                tracing::error!(target: "ternvale::firmware", path = %path.display(), what, error = %source, "nvram file error");
                FirmwareError::Nvram { what, path, source }
            }
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(fail("create directory for"))?;
        }
        let existed = path.exists();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(fail("open"))?;
        let bytes = file.metadata().map_err(fail("stat"))?.len();
        if bytes > bank.len() as u64 {
            tracing::error!(target: "ternvale::firmware", path = %path.display(), bytes, "nvram file is larger than the flash bank");
            return Err(FirmwareError::NvramTooLarge {
                path: path.to_path_buf(),
                bytes,
                limit: bank.len() as u64,
            });
        }
        file.read_exact_at(&mut bank[..bytes as usize], 0)
            .map_err(fail("read"))?;
        tracing::info!(
            target: "ternvale::firmware",
            path = %path.display(),
            bytes = format!("{bytes:#x}"),
            created = !existed,
            "opened nvram file"
        );
        Ok(Self {
            file,
            path: path.to_path_buf(),
            dirty: false,
        })
    }

    /// Write `bytes` at `offset`. The file grows as needed.
    pub(super) fn persist(&mut self, offset: u64, bytes: &[u8]) -> Result<(), FirmwareError> {
        self.file.write_all_at(bytes, offset).map_err(|source| {
            tracing::error!(target: "ternvale::firmware", path = %self.path.display(), offset, error = %source, "nvram write failed");
            FirmwareError::Nvram {
                what: "write",
                path: self.path.clone(),
                source,
            }
        })?;
        self.dirty = true;
        tracing::trace!(
            target: "ternvale::firmware",
            offset = format!("{offset:#x}"),
            len = bytes.len(),
            "nvram write-through"
        );
        Ok(())
    }

    /// `fsync` if anything was written.
    pub(super) fn sync(&mut self) {
        if !self.dirty {
            return;
        }
        match self.file.sync_all() {
            Ok(()) => {
                self.dirty = false;
                tracing::info!(target: "ternvale::firmware", path = %self.path.display(), "nvram file synced");
            }
            Err(error) => tracing::error!(
                target: "ternvale::firmware",
                path = %self.path.display(),
                error = %error,
                "nvram sync failed"
            ),
        }
    }
}
