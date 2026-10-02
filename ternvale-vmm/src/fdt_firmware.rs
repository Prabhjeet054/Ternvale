//! DT nodes for the PL031 RTC and, on a firmware boot, the CFI flash banks.
//!
//! Both match QEMU `virt` (`create_rtc`, `create_one_flash` with
//! `virt_flash_fdt`'s single node covering two banks). EDK2's
//! `NorFlashQemuLib` walks `cfi-flash` `reg` entries, skips the one holding
//! the running firmware volume, and marks the node `disabled` before handing
//! the DT to the OS, so Linux never probes the banks.

use vm_fdt::{FdtWriter, FdtWriterResult};

use super::reg;
use crate::platform::{FLASH_BANK_SIZE, FLASH_CODE_BASE, FLASH_VARS_BASE, RTC_BASE};
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
