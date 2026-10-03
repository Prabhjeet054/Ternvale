//! What the ACPI tables must say, from the platform map, and the check that
//! the tables in guest memory say it.
//!
//! [`acpi_config`] takes every address and interrupt from the same constants
//! the DTB uses ([`crate::platform`], the affinity fields of
//! [`crate::smp::mpidr`] (the DTB `cpu` `reg`), the DTB's UART SPI,
//! timer PPIs, and PL011 register size, and the ECAM bus count), so both boot
//! paths describe one machine. [`check`] runs after the tables are written:
//! it decodes them back out of guest memory and fails the boot with
//! [`ternvale_acpi::AcpiError::Drift`] if any table, or the config itself,
//! disagrees with the [`Layout::virt`] regions.

use ternvale_acpi::{
    AcpiConfig, AcpiError, DumpedTable, EcamConfig, GicConfig, PsciConduit, TimerConfig, UartConfig,
};

use crate::fdt::{PL011_REG_SIZE, TIMER_ALWAYS_ON, TIMER_PPIS, UART_SPI};
use crate::gic_redist::REDIST_STRIDE;
use crate::machine::MachineError;
use crate::pci::ECAM_BUSES;
use crate::platform::{
    Layout, ACPI_BASE, ACPI_SIZE, GIC_DIST_BASE, GIC_REDIST_BASE, GIC_REDIST_SIZE, PCIE_ECAM_BASE,
    UART_BASE,
};
use crate::smp::MPIDR_AFFINITY_MASK;

/// ECAM bytes per bus (PCIe Base Specification §7.2.2: 32 devices × 8
/// functions × 4 KiB).
const ECAM_BUS_BYTES: u64 = 1 << 20;
/// GICv3 distributor register frame (GICv3 architecture: 64 KiB).
const GICD_FRAME: u64 = 0x1_0000;

/// The tables' view of the platform for a guest with `cpus` vCPUs.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(cpus))]
pub fn acpi_config(cpus: u32) -> AcpiConfig {
    AcpiConfig {
        conduit: PsciConduit::Hvc,
        mpidrs: (0..cpus)
            .map(|cpu| crate::smp::mpidr(cpu) & MPIDR_AFFINITY_MASK)
            .collect(),
        gic: GicConfig {
            dist_base: GIC_DIST_BASE,
            redist_base: GIC_REDIST_BASE,
            redist_len: GIC_REDIST_SIZE as u32,
        },
        timer: TimerConfig {
            ppis: TIMER_PPIS,
            always_on: TIMER_ALWAYS_ON,
        },
        ecam: EcamConfig {
            base: PCIE_ECAM_BASE,
            start_bus: 0,
            end_bus: ECAM_BUSES - 1,
        },
        uart: UartConfig {
            base: UART_BASE,
            len: PL011_REG_SIZE as u32,
            spi: UART_SPI,
        },
    }
}

/// One line when region `name` of `layout` fails `ok(base, size)`.
fn region_problem(
    layout: &Layout,
    what: &str,
    name: &str,
    ok: impl Fn(u64, u64) -> bool,
    found: String,
) -> Option<String> {
    match layout.regions().iter().find(|r| r.name == name) {
        Some(r) if ok(r.base, r.size) => None,
        Some(r) => Some(format!(
            "{what}: platform {name} is {:#x}+{:#x}, tables say {found}",
            r.base, r.size
        )),
        None => Some(format!("{what}: platform has no {name} region")),
    }
}

/// Lines where `config` disagrees with the `layout` regions.
fn platform_problems(config: &AcpiConfig, layout: &Layout) -> Vec<String> {
    let gic = config.gic;
    let ecam = config.ecam;
    let buses = u64::from(ecam.end_bus) - u64::from(ecam.start_bus) + 1;
    let uart = config.uart;
    let uart_end = uart.base.saturating_add(u64::from(uart.len));
    let mut problems: Vec<String> = [
        region_problem(
            layout,
            "GICD",
            "gic-dist",
            |base, size| base == gic.dist_base && size >= GICD_FRAME,
            format!("{:#x}", gic.dist_base),
        ),
        region_problem(
            layout,
            "GICR",
            "gic-redist",
            |base, size| base == gic.redist_base && size == u64::from(gic.redist_len),
            format!("{:#x}+{:#x}", gic.redist_base, gic.redist_len),
        ),
        region_problem(
            layout,
            "MCFG",
            "pcie-ecam",
            |base, size| base == ecam.base && size == buses * ECAM_BUS_BYTES,
            format!(
                "{:#x} buses {}..={}",
                ecam.base, ecam.start_bus, ecam.end_bus
            ),
        ),
        region_problem(
            layout,
            "SPCR/DBG2 UART",
            "uart",
            |base, size| uart.base >= base && uart_end <= base.saturating_add(size),
            format!("{:#x}+{:#x}", uart.base, uart.len),
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    let need = config.mpidrs.len() as u64 * REDIST_STRIDE;
    if need > u64::from(gic.redist_len) {
        problems.push(format!(
            "GICR: {} cpus need {need:#x} bytes of redistributors, range is {:#x}",
            config.mpidrs.len(),
            gic.redist_len
        ));
    }
    let dtb_timer = (TIMER_PPIS, TIMER_ALWAYS_ON);
    if (config.timer.ppis, config.timer.always_on) != dtb_timer {
        problems.push(format!(
            "GTDT: tables use timer PPIs {:?} always-on {}, DTB timer node uses {:?} always-on {}",
            config.timer.ppis, config.timer.always_on, dtb_timer.0, dtb_timer.1
        ));
    }
    if config.uart.spi != UART_SPI || u64::from(config.uart.len) != PL011_REG_SIZE {
        problems.push(format!(
            "SPCR/DBG2: tables use uart SPI {} size {:#x}, DTB pl011 uses SPI {UART_SPI} size \
             {PL011_REG_SIZE:#x}",
            config.uart.spi, config.uart.len
        ));
    }
    problems
}

/// Fail unless `tables` (read back from guest memory) describe the platform
/// for `cpus` vCPUs and sit inside the ACPI window.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(cpus, tables = tables.len()))]
pub(crate) fn check(tables: &[DumpedTable], cpus: u32) -> Result<(), MachineError> {
    let config = acpi_config(cpus);
    let layout = Layout::virt(0);
    let mut problems = platform_problems(&config, &layout);
    for table in tables {
        let end = table.gpa.saturating_add(table.bytes.len() as u64);
        if table.gpa < ACPI_BASE || end > ACPI_BASE + ACPI_SIZE {
            problems.push(format!(
                "{}: at {:#x}..{end:#x}, outside the acpi window",
                table.signature, table.gpa
            ));
        }
    }
    for problem in &problems {
        tracing::error!(target: "ternvale::acpi", problem = %problem, "acpi platform drift");
    }
    match ternvale_acpi::verify(tables, &config) {
        Ok(()) => {}
        Err(AcpiError::Drift { problems: found }) => problems.extend(found),
        Err(other) => return Err(other.into()),
    }
    if !problems.is_empty() {
        tracing::error!(
            target: "ternvale::acpi",
            count = problems.len(),
            "acpi tables do not match platform.rs; refusing to boot"
        );
        return Err(AcpiError::Drift { problems }.into());
    }
    tracing::info!(
        target: "ternvale::acpi",
        cpus,
        tables = tables.len(),
        "acpi tables match the platform map"
    );
    Ok(())
}

#[cfg(test)]
#[path = "acpi_check_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "acpi_dtb_tests.rs"]
mod dtb_tests;
