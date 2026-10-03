//! UEFI firmware boot: EDK2 ArmVirtQemu on the QEMU `virt` flash layout.
//!
//! Requirements checked against edk2-stable202308 (`ArmVirtQemu.dsc`/`.fdf`,
//! `VarStore.fdf.inc`, `NorFlashQemuLib`, `VirtNorFlashDxe`):
//!
//! - The code image (`QEMU_EFI.fd`) runs in place from GPA 0
//!   (`PcdFdBaseAddress = 0`). It is mapped read and execute in flash bank 0.
//! - The variable store lives at `0x04000000` (bank 1): three 256 KiB blocks
//!   (variables, FTW working, FTW spare). It is CFI NOR flash driven by
//!   [`VarsFlash`] and persisted to the per-VM NVRAM file.
//! - RAM starts at `0x40000000` and is at least 128 MiB. The DTB sits at the
//!   first byte of RAM (`PcdDeviceTreeInitialBaseAddress`) and must end below
//!   the boot stack at `0x4007c000`.
//! - The DT needs a `cfi-flash` node, an `arm,pl031` RTC, the PL011 behind
//!   `/chosen/stdout-path`, GICv3, the arch timer, PSCI, and optionally PCIe.
//! - `fw_cfg` is not provided. Every ArmVirtQemu consumer handles a missing
//!   `qemu,fw-cfg-mmio` node: no ACPI tables (the DT is passed on instead), no
//!   QEMU boot order, no kernel loading.
//!   TODO(verify): EDK2 master since 2026-08-28 ("ArmVirtPkg: Map QEMU fw_cfg
//!   MMIO region in PEI") asserts in `ArmVirtGetMemoryMap` without that node.
//!   Released EDK2 up to edk2-stable202608 does not.

mod nvram;
mod pflash;
mod vars;

use crate::memory::{GuestMemory, MemoryError};
use crate::platform::{FLASH_BANK_SIZE, FLASH_CODE_BASE, RAM_BASE};

pub use vars::{HvWindow, RomdWindow, VarsFlash};

/// EDK2 asserts at least 128 MiB (`ArmVirtPkg` memory initialization).
pub const MIN_RAM: u64 = 128 << 20;
/// `PcdCPUCoresStackBase` in ArmVirtQemu: the DTB must end below it.
pub const DTB_LIMIT: u64 = 0x4007_c000;

/// Firmware loading or the variable store failed.
#[derive(Debug, thiserror::Error)]
pub enum FirmwareError {
    /// The code image is empty or larger than one flash bank.
    #[error("firmware image is {bytes:#x} bytes; it must be 1..={limit:#x}")]
    ImageSize {
        /// Image length.
        bytes: u64,
        /// One flash bank.
        limit: u64,
    },
    /// Guest RAM is smaller than EDK2 accepts.
    #[error("firmware boot needs at least {min:#x} bytes of RAM, got {ram:#x}")]
    RamTooSmall {
        /// Configured RAM.
        ram: u64,
        /// [`MIN_RAM`].
        min: u64,
    },
    /// The DTB would overlap EDK2's boot stack.
    #[error("dtb of {bytes:#x} bytes at {base:#x} crosses {limit:#x}")]
    DtbTooLarge {
        /// DTB length.
        bytes: u64,
        /// [`RAM_BASE`].
        base: u64,
        /// [`DTB_LIMIT`].
        limit: u64,
    },
    /// The NVRAM file is larger than the variable bank.
    #[error("nvram {} is {bytes:#x} bytes; the bank holds {limit:#x}", path.display())]
    NvramTooLarge {
        /// NVRAM file.
        path: std::path::PathBuf,
        /// File length.
        bytes: u64,
        /// One flash bank.
        limit: u64,
    },
    /// Opening, reading, or writing the NVRAM file failed.
    #[error("nvram {what} {}: {source}", path.display())]
    Nvram {
        /// Operation, for example `open`.
        what: &'static str,
        /// NVRAM file.
        path: std::path::PathBuf,
        /// OS error.
        source: std::io::Error,
    },
    /// Guest memory rejected a firmware region or write.
    #[error("firmware {what}: {source}")]
    Memory {
        /// Operation, for example `map code bank`.
        what: &'static str,
        /// Memory error.
        source: MemoryError,
    },
    /// The DTB could not be written at [`RAM_BASE`].
    #[error("firmware dtb: {0}")]
    Fdt(#[from] crate::fdt::FdtError),
    /// Setting the boot CPU's entry registers failed.
    #[error("firmware entry registers: {0}")]
    Regs(#[from] crate::linux::LinuxBootError),
    /// `hv_vm_map` / `hv_vm_unmap` of the variable window failed.
    #[error("flash window {what} at {gpa:#x}: {source}")]
    Window {
        /// `map` or `unmap`.
        what: &'static str,
        /// Bank base.
        gpa: u64,
        /// Hypervisor error.
        source: ternvale_hv::HvError,
    },
    /// A fw_cfg file cannot be offered (bad name, too large, too many).
    #[error("fw_cfg file {name:?}: {reason}")]
    FwCfgFile {
        /// File name.
        name: String,
        /// What is wrong.
        reason: String,
    },
}

/// Map `image` read and execute at GPA 0, in a bank-sized region. Bytes after
/// the image read as zero, like the zero padding QEMU users add to reach 64 MiB.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::firmware",
    skip_all,
    fields(bytes = image.len())
)]
pub fn map_code(
    memory: &mut GuestMemory,
    vm: &ternvale_hv::Vm,
    image: &[u8],
) -> Result<(), FirmwareError> {
    let bytes = image.len() as u64;
    if image.is_empty() || bytes > FLASH_BANK_SIZE {
        tracing::error!(target: "ternvale::firmware", bytes, limit = FLASH_BANK_SIZE, "rejected firmware image size");
        return Err(FirmwareError::ImageSize {
            bytes,
            limit: FLASH_BANK_SIZE,
        });
    }
    let flags = ternvale_hv::HV_MEMORY_READ | ternvale_hv::HV_MEMORY_EXEC;
    memory
        .map_flags(vm, FLASH_CODE_BASE, FLASH_BANK_SIZE, flags)
        .map_err(|source| FirmwareError::Memory {
            what: "map code bank",
            source,
        })?;
    memory
        .write_bytes(FLASH_CODE_BASE, image)
        .map_err(|source| FirmwareError::Memory {
            what: "copy code image",
            source,
        })?;
    let first = image
        .get(..4)
        .and_then(|word| <[u8; 4]>::try_from(word).ok())
        .map_or(0, u32::from_le_bytes);
    tracing::info!(
        target: "ternvale::firmware",
        gpa = format!("{FLASH_CODE_BASE:#x}"),
        bytes = format!("{bytes:#x}"),
        bank = format!("{FLASH_BANK_SIZE:#x}"),
        sha256 = %sha256_hex(image),
        first_insn = format!("{first:#010x}"),
        "mapped firmware code (read+exec)"
    );
    Ok(())
}

/// Check the EDK2 RAM and DTB constraints before placing the DTB at [`RAM_BASE`].
#[tracing::instrument(
    level = "debug",
    target = "ternvale::firmware",
    skip_all,
    fields(ram, dtb)
)]
pub fn check_layout(ram: u64, dtb: u64) -> Result<(), FirmwareError> {
    if ram < MIN_RAM {
        tracing::error!(target: "ternvale::firmware", ram = format!("{ram:#x}"), "firmware boot needs at least 128 MiB of RAM");
        return Err(FirmwareError::RamTooSmall { ram, min: MIN_RAM });
    }
    if RAM_BASE.saturating_add(dtb) > DTB_LIMIT {
        tracing::error!(target: "ternvale::firmware", dtb, "dtb overlaps the EDK2 boot stack");
        return Err(FirmwareError::DtbTooLarge {
            bytes: dtb,
            base: RAM_BASE,
            limit: DTB_LIMIT,
        });
    }
    tracing::debug!(target: "ternvale::firmware", ram = format!("{ram:#x}"), dtb, "firmware layout accepted");
    Ok(())
}

/// Copy the DTB to [`RAM_BASE`] and point the boot CPU at the firmware reset
/// vector: `PC = 0`, EL1h with DAIF masked, MMU off. `x0` holds the DTB
/// address as for Linux; ArmVirtQemu reads the fixed PCD instead, so this is
/// only a convenience.
#[tracing::instrument(level = "debug", target = "ternvale::firmware", skip_all, fields(dtb = dtb.len()))]
pub fn enter(
    memory: &mut GuestMemory,
    cpu: &impl crate::linux::BootRegs,
    dtb: &[u8],
    ram: u64,
) -> Result<(), FirmwareError> {
    check_layout(ram, dtb.len() as u64)?;
    crate::fdt::write_fdt(memory, RAM_BASE, dtb, None)?;
    cpu.set_gpr(0, RAM_BASE)?;
    for index in 1..4 {
        cpu.set_gpr(index, 0)?;
    }
    cpu.set_pc(FLASH_CODE_BASE)?;
    cpu.set_cpsr(crate::linux::CPSR_EL1H_MASKED)?;
    tracing::info!(
        target: "ternvale::firmware",
        pc = format!("{FLASH_CODE_BASE:#x}"),
        x0 = format!("{RAM_BASE:#x}"),
        cpsr = format!("{:#x}", crate::linux::CPSR_EL1H_MASKED),
        "boot cpu enters the firmware reset vector"
    );
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests;
