//! Variable-store flash bank: CFI command state, backing pages, and NVRAM.
//!
//! In read-array mode the backing pages are mapped into the guest read-only,
//! so EDK2's `CopyMem` (which may use `LDP` or SIMD loads that do not report a
//! decodable syndrome) reads memory directly, and any store takes a stage-2
//! permission fault that reaches the MMIO bus as a command. Every other mode
//! unmaps the window so status and ID reads trap too. QEMU calls this "romd"
//! mode (`memory_region_rom_device_set_romd`).

use std::path::Path;
use std::ptr::NonNull;

use super::nvram::NvramFile;
use super::pflash::{Cfi, Changed};
use super::FirmwareError;
use crate::memory::HostPages;
use crate::mmio::MmioDevice;
use crate::platform::FLASH_BANK_SIZE;

/// Maps the bank's backing pages into the guest read-only, or removes them.
pub trait RomdWindow: Send {
    /// Map `len` bytes at `host` read-only.
    fn map(&mut self, host: NonNull<u8>, len: usize) -> Result<(), FirmwareError>;
    /// Remove the mapping of `len` bytes.
    fn unmap(&mut self, len: usize) -> Result<(), FirmwareError>;
}

/// [`RomdWindow`] over `hv_vm_map` / `hv_vm_unmap` at a fixed GPA.
#[derive(Debug)]
pub struct HvWindow {
    gpa: u64,
}

impl HvWindow {
    /// A window at `gpa`, initially unmapped.
    pub fn new(gpa: u64) -> Self {
        Self { gpa }
    }
}

impl RomdWindow for HvWindow {
    fn map(&mut self, host: NonNull<u8>, len: usize) -> Result<(), FirmwareError> {
        ternvale_hv::map_memory(host, self.gpa, len, ternvale_hv::HV_MEMORY_READ).map_err(
            |source| FirmwareError::Window {
                what: "map",
                gpa: self.gpa,
                source,
            },
        )
    }

    fn unmap(&mut self, len: usize) -> Result<(), FirmwareError> {
        ternvale_hv::unmap_memory(self.gpa, len).map_err(|source| FirmwareError::Window {
            what: "unmap",
            gpa: self.gpa,
            source,
        })
    }
}

/// The UEFI variable store (QEMU `pflash1`). Register it on the MMIO bus at
/// [`crate::platform::FLASH_VARS_BASE`] with size [`FLASH_BANK_SIZE`].
pub struct VarsFlash {
    cfi: Cfi,
    storage: HostPages,
    window: Box<dyn RomdWindow>,
    mapped: bool,
    nvram: NvramFile,
}

impl VarsFlash {
    /// Load `nvram` (creating it if missing) and map the bank read-only.
    #[tracing::instrument(level = "debug", target = "ternvale::flash", skip_all, fields(nvram = %nvram.display()))]
    pub fn open(nvram: &Path, window: Box<dyn RomdWindow>) -> Result<Self, FirmwareError> {
        Self::with_size(nvram, window, FLASH_BANK_SIZE)
    }

    pub(super) fn with_size(
        nvram: &Path,
        window: Box<dyn RomdWindow>,
        size: u64,
    ) -> Result<Self, FirmwareError> {
        let mut storage = HostPages::new(size).map_err(|source| FirmwareError::Memory {
            what: "allocate variable bank",
            source,
        })?;
        let nvram = NvramFile::open(nvram, storage.as_mut_slice())?;
        let mut flash = Self {
            cfi: Cfi::new(),
            storage,
            window,
            mapped: false,
            nvram,
        };
        flash.try_sync_window()?;
        tracing::info!(target: "ternvale::flash", size = format!("{size:#x}"), "variable flash ready (read-array, mapped read-only)");
        Ok(flash)
    }

    fn sync_window(&mut self) {
        if let Err(error) = self.try_sync_window() {
            tracing::error!(
                target: "ternvale::flash",
                mapped = self.mapped,
                error = %error,
                "variable flash window change failed; accesses keep trapping"
            );
        }
    }

    fn try_sync_window(&mut self) -> Result<(), FirmwareError> {
        let want = self.cfi.array_mode();
        if want == self.mapped {
            return Ok(());
        }
        let len = self.storage.len();
        if want {
            self.window.map(self.storage.host(), len)?;
        } else {
            self.window.unmap(len)?;
        }
        self.mapped = want;
        tracing::debug!(target: "ternvale::flash", mapped = want, "variable flash window");
        Ok(())
    }

    fn persist(&mut self, changed: Changed) {
        let end = changed.offset as usize + changed.len;
        let Some(bytes) = self.storage.as_slice().get(changed.offset as usize..end) else {
            return;
        };
        let bytes = bytes.to_vec();
        if self.nvram.persist(changed.offset, &bytes).is_err() {
            self.cfi.fail_program();
        }
    }
}

impl MmioDevice for VarsFlash {
    fn name(&self) -> &str {
        "pflash-vars"
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        let value = self.cfi.read(self.storage.as_slice(), offset, size);
        tracing::trace!(
            target: "ternvale::flash",
            offset = format!("{offset:#x}"),
            size,
            value = format!("{value:#x}"),
            mode = ?self.cfi.mode(),
            "flash read"
        );
        value
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        let before = self.cfi.mode();
        match self
            .cfi
            .write(self.storage.as_mut_slice(), offset, size, val)
        {
            Ok(Some(changed)) => {
                tracing::debug!(
                    target: "ternvale::flash",
                    offset = format!("{:#x}", changed.offset),
                    len = changed.len,
                    "flash contents changed"
                );
                self.persist(changed);
            }
            Ok(None) => {}
            Err(reason) => tracing::warn!(
                target: "ternvale::flash",
                offset = format!("{offset:#x}"),
                size,
                value = format!("{val:#x}"),
                reason,
                "rejected flash write"
            ),
        }
        tracing::trace!(
            target: "ternvale::flash",
            offset = format!("{offset:#x}"),
            size,
            value = format!("{val:#x}"),
            from = ?before,
            to = ?self.cfi.mode(),
            status = format!("{:#x}", self.cfi.status()),
            "flash write"
        );
        self.sync_window();
    }
}

impl Drop for VarsFlash {
    fn drop(&mut self) {
        if self.mapped {
            if let Err(error) = self.window.unmap(self.storage.len()) {
                tracing::error!(target: "ternvale::flash", error = %error, "unmap variable flash on drop failed");
            }
        }
        self.nvram.sync();
    }
}
