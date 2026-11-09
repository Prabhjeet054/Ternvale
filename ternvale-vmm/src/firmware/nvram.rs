//! The per-VM NVRAM file behind the variable-store flash bank.
//!
//! The file holds the first `len` bytes of the bank; anything after it reads
//! as zero, like a zero-padded `QEMU_VARS.fd`. A missing file is created
//! empty: EDK2 then sees no valid firmware-volume header, erases the store,
//! and writes fresh headers (`VirtNorFlashDxe.c`). Each program or erase is
//! written through with `pwrite`; the file is synced when the bank is dropped.
//! With encryption the whole bank is rewritten as an AES-256-GCM envelope.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use ternvale_crypto::{is_envelope, GcmKey, AAD_NVRAM};

use super::FirmwareError;

/// An open NVRAM file.
pub(super) struct NvramFile {
    file: File,
    path: PathBuf,
    dirty: bool,
    gcm: Option<GcmKey>,
}

impl NvramFile {
    /// Open or create `path` and copy its contents into `bank`.
    #[tracing::instrument(level = "debug", target = "ternvale::firmware", skip_all, fields(path = %path.display(), encrypted = gcm.is_some()))]
    pub(super) fn open(
        path: &Path,
        bank: &mut [u8],
        gcm: Option<GcmKey>,
    ) -> Result<Self, FirmwareError> {
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
        let raw = std::fs::read(path).map_err(fail("read"))?;
        if !raw.is_empty() {
            let plain = if is_envelope(&raw) {
                let Some(key) = gcm.as_ref() else {
                    tracing::warn!(
                        target: "ternvale::firmware",
                        path = %path.display(),
                        "NVRAM is GCM-protected but no data key is loaded"
                    );
                    return Err(FirmwareError::Crypto(ternvale_crypto::CryptoError::Auth {
                        what: "nvram-missing-key",
                    }));
                };
                key.open_at(&raw, AAD_NVRAM, Some(path))?
            } else {
                raw
            };
            if plain.len() as u64 > bank.len() as u64 {
                tracing::error!(target: "ternvale::firmware", path = %path.display(), bytes = plain.len(), "nvram file is larger than the flash bank");
                return Err(FirmwareError::NvramTooLarge {
                    path: path.to_path_buf(),
                    bytes: plain.len() as u64,
                    limit: bank.len() as u64,
                });
            }
            bank[..plain.len()].copy_from_slice(&plain);
        }
        tracing::info!(
            target: "ternvale::firmware",
            path = %path.display(),
            created = !existed,
            encrypted = gcm.is_some(),
            "opened nvram file"
        );
        Ok(Self {
            file,
            path: path.to_path_buf(),
            dirty: false,
            gcm,
        })
    }

    pub(super) fn encrypted(&self) -> bool {
        self.gcm.is_some()
    }

    /// Write `bytes` at `offset`. The file grows as needed.
    pub(super) fn persist(&mut self, offset: u64, bytes: &[u8]) -> Result<(), FirmwareError> {
        if self.gcm.is_some() {
            return Ok(());
        }
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

    /// Rewrite the whole bank as an AES-256-GCM envelope.
    pub(super) fn persist_bank(&mut self, bank: &[u8]) -> Result<(), FirmwareError> {
        let Some(gcm) = &self.gcm else {
            return Ok(());
        };
        ternvale_crypto::write_gcm_file(gcm, &self.path, bank, AAD_NVRAM)?;
        self.dirty = false;
        tracing::debug!(
            target: "ternvale::firmware",
            path = %self.path.display(),
            len = bank.len(),
            "nvram GCM envelope rewritten"
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
