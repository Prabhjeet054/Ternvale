//! DT nodes for the PL031 RTC and, on a firmware boot, the CFI flash banks
//! and (with ACPI tables) fw_cfg and the TPM.
//!
//! All match QEMU `virt` (`create_rtc`, `create_one_flash` with
//! `virt_flash_fdt`'s single node covering two banks, `create_fw_cfg`).
//! EDK2's `NorFlashQemuLib` walks `cfi-flash` `reg` entries, skips the one
//! holding the running firmware volume, and marks the node `disabled` before
//! handing the DT to the OS, so Linux never probes the banks.

use vm_fdt::{FdtWriter, FdtWriterResult};

use ternvale_tpm::CRB_WINDOW_SIZE;

use super::reg;
use crate::fw_cfg::FW_CFG_REG_SIZE;
use crate::platform::{
    FLASH_BANK_SIZE, FLASH_CODE_BASE, FLASH_VARS_BASE, FW_CFG_BASE, RTC_BASE, TPM_CRB_BASE,
};
use crate::rtc::{PL031_REG_SIZE, RTC_SPI};

/// GIC SPI, level-high.
const IRQ_LEVEL_HIGH: u32 = 4;
/// Two interleaved x16 chips: 4-byte bank.
const BANK_WIDTH: u32 = 4;

pub(super) fn rtc(w: &mut FdtWriter, clock: u32) -> FdtWriterResult<()> {
    let node = w.begin_node(&format!("pl031@{RTC_BASE:x}"))?;
    w.property_string_list(
        "compatible",
        vec!["arm,pl031".to_string(), "arm,primecell".to_string()],
    )?;
    w.property_array_u32("reg", &reg(RTC_BASE, PL031_REG_SIZE))?;
    w.property_array_u32("interrupts", &[0, RTC_SPI, IRQ_LEVEL_HIGH])?;
    w.property_u32("clocks", clock)?;
    w.property_string("clock-names", "apb_pclk")?;
    w.end_node(node)
}

pub(super) fn flash(w: &mut FdtWriter) -> FdtWriterResult<()> {
    let node = w.begin_node(&format!("flash@{FLASH_CODE_BASE:x}"))?;
    w.property_string("compatible", "cfi-flash")?;
    let mut cells = reg(FLASH_CODE_BASE, FLASH_BANK_SIZE).to_vec();
    cells.extend_from_slice(&reg(FLASH_VARS_BASE, FLASH_BANK_SIZE));
    w.property_array_u32("reg", &cells)?;
    w.property_u32("bank-width", BANK_WIDTH)?;
    tracing::debug!(
        target: "ternvale::boot",
        code = format!("{FLASH_CODE_BASE:#x}"),
        vars = format!("{FLASH_VARS_BASE:#x}"),
        bank = format!("{FLASH_BANK_SIZE:#x}"),
        "dtb cfi-flash node"
    );
    w.end_node(node)
}

/// EDK2 `QemuFwCfgInitialize` asserts 2 address and 2 size cells and one
/// `reg` entry. A size of `0x10` advertises no DMA register (see `fw_cfg.rs`).
/// QEMU also sets `dma-coherent`, which is left out because there is no DMA.
pub(super) fn fw_cfg(w: &mut FdtWriter) -> FdtWriterResult<()> {
    let node = w.begin_node(&format!("fw-cfg@{FW_CFG_BASE:x}"))?;
    w.property_string("compatible", "qemu,fw-cfg-mmio")?;
    w.property_array_u32("reg", &reg(FW_CFG_BASE, FW_CFG_REG_SIZE))?;
    tracing::debug!(
        target: "ternvale::boot",
        base = format!("{FW_CFG_BASE:#x}"),
        size = format!("{FW_CFG_REG_SIZE:#x}"),
        "dtb fw-cfg node"
    );
    w.end_node(node)
}

/// The TPM CRB, for EDK2 only. ArmVirtPkg `PlatformPeiLib` (edk2-stable202308)
/// sets `PcdTpmBaseAddress` from the first top-level `tcg,tpm-tis-mmio` node
/// and installs the TPM-discovered PPI, which starts `Tcg2ConfigPei` and
/// `Tcg2Pei`. Their `Tpm2DeviceLibDTpm` reads `TPM_CRB_INTF_ID` and picks the
/// CRB interface itself, so the TIS compatible is only the discovery key; QEMU
/// `virt` uses it for its own TPM node. Only `acpi`-mode trees carry it: there
/// EDK2 hands the OS ACPI (the TPM2 table and `\_SB.TPM2`), never this DT. A
/// DT-booted Linux would bind `tpm_tis` to it, which cannot drive a CRB.
/// TODO(verify): newer EDK2 still keys TPM discovery on this compatible.
pub(super) fn tpm(w: &mut FdtWriter) -> FdtWriterResult<()> {
    let node = w.begin_node(&format!("tpm@{TPM_CRB_BASE:x}"))?;
    w.property_string("compatible", "tcg,tpm-tis-mmio")?;
    w.property_array_u32("reg", &reg(TPM_CRB_BASE, CRB_WINDOW_SIZE))?;
    tracing::debug!(
        target: "ternvale::boot",
        base = format!("{TPM_CRB_BASE:#x}"),
        size = format!("{CRB_WINDOW_SIZE:#x}"),
        "dtb tpm node for firmware discovery"
    );
    w.end_node(node)
}
