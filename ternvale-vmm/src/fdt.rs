//! Guest device tree for the Linux arm64 boot protocol.
//!
//! Addresses come from [`crate::platform`]. The PL011 `reg` size is the 4 KiB
//! register block. The platform reservation around it is one host page.

use std::path::Path;
use std::process::Command;

use vm_fdt::{FdtWriter, FdtWriterResult};

use crate::memory::{GuestMemory, MemoryError};
use crate::platform::{
    GIC_DIST_BASE, GIC_DIST_SIZE, GIC_REDIST_BASE, GIC_REDIST_SIZE, UART_BASE, VIRTIO_MMIO_BASE,
    VIRTIO_MMIO_SLOTS, VIRTIO_MMIO_SLOT_SIZE,
};

/// PL011 register block. QEMU's `VIRT_UART` size, not the 16 KiB reservation.
pub const PL011_REG_SIZE: u64 = 0x1000;
/// 24 MHz fixed clock. QEMU virt `apb-pclk` is `0x16e3600`.
const APB_HZ: u32 = 24_000_000;
/// First virtio-mmio SPI. Matches the dumped QEMU virt tree.
const VIRTIO_SPI0: u32 = 0x10;
/// GIC SPI, level-high. QEMU's PL011 interrupt.
pub const UART_SPI: u32 = 1;

/// Inputs for one guest DTB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestFdt {
    /// Kernel command line, written to `/chosen/bootargs`.
    pub bootargs: String,
    /// First byte of guest RAM.
    pub ram_base: u64,
    /// Guest RAM length.
    pub ram_size: u64,
    /// First byte of the initramfs. `linux,initrd-start`.
    pub initrd_start: u64,
    /// Byte after the initramfs. `linux,initrd-end`.
    pub initrd_end: u64,
    /// Number of CPU nodes. Each uses `enable-method = "psci"`.
    pub cpu_count: u32,
}

/// Building or installing the DTB failed.
#[derive(Debug, thiserror::Error)]
pub enum FdtError {
    /// `vm-fdt` rejected the tree.
    #[error("build guest dtb: {0}")]
    Build(#[from] vm_fdt::Error),
    /// No CPU nodes were requested.
    #[error("guest dtb needs at least one cpu")]
    NoCpus,
    /// The blob did not fit in guest RAM.
    #[error("write dtb at {gpa:#x}: {source}")]
    Memory {
        /// Destination GPA.
        gpa: u64,
        /// Guest memory error.
        source: MemoryError,
    },
    /// `TERNVALE_DUMP_DTB=1` but the DTB file could not be created.
    #[error("write dtb dump {path}: {source}")]
    Dump {
        /// Path that failed.
        path: String,
        /// OS error.
        source: std::io::Error,
    },
}

/// Build the DTB blob.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::boot",
    skip_all,
    fields(cpus = fdt.cpu_count, ram_size = format!("{:#x}", fdt.ram_size))
)]
pub fn build_fdt(fdt: &GuestFdt) -> Result<Vec<u8>, FdtError> {
    if fdt.cpu_count == 0 {
        tracing::warn!(target: "ternvale::boot", "rejected guest dtb with no cpus");
        return Err(FdtError::NoCpus);
    }
    let mut w = FdtWriter::new()?;
    let root = w.begin_node("")?;
    w.property_u32("#address-cells", 2)?;
    w.property_u32("#size-cells", 2)?;
    w.property_string("compatible", "linux,dummy-virt")?;
    w.property_string("model", "ternvale,virt")?;
    let clock = 1;
    let gic = 2;
    w.property_u32("interrupt-parent", gic)?;
    chosen(&mut w, fdt)?;
    memory(&mut w, fdt)?;
    cpus(&mut w, fdt.cpu_count)?;
    psci(&mut w)?;
    timer(&mut w)?;
    intc(&mut w, gic)?;
    uart(&mut w, clock)?;
    apb_clock(&mut w, clock)?;
    virtio(&mut w)?;
    w.end_node(root)?;
    let blob = w.finish()?;
    tracing::info!(target: "ternvale::boot", bytes = blob.len(), "built guest dtb");
    Ok(blob)
}

/// Copy `blob` to `gpa` and log its size. When `TERNVALE_DUMP_DTB=1` and
/// `log_dir` is set, write `guest.dtb` and a `dtc` decompile named `guest.dts`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::boot",
    skip_all,
    fields(gpa = format!("{:#x}", gpa), bytes = blob.len())
)]
pub fn write_fdt(
    memory: &mut GuestMemory,
    gpa: u64,
    blob: &[u8],
    log_dir: Option<&Path>,
) -> Result<(), FdtError> {
    memory.write_bytes(gpa, blob).map_err(|source| {
        tracing::error!(
            target: "ternvale::boot",
            gpa = format!("{:#x}", gpa),
            error = %source,
            "failed to write guest dtb"
        );
        FdtError::Memory { gpa, source }
    })?;
    tracing::info!(
        target: "ternvale::boot",
        gpa = format!("{:#x}", gpa),
        bytes = blob.len(),
        "wrote guest dtb"
    );
    if std::env::var("TERNVALE_DUMP_DTB").ok().as_deref() == Some("1") {
        if let Some(dir) = log_dir {
            dump_dtb(dir, blob)?;
        } else {
            tracing::warn!(target: "ternvale::boot", "TERNVALE_DUMP_DTB=1 but no log directory was set");
        }
    }
    Ok(())
}

fn chosen(w: &mut FdtWriter, fdt: &GuestFdt) -> FdtWriterResult<()> {
    let node = w.begin_node("chosen")?;
    w.property_string("bootargs", &fdt.bootargs)?;
    w.property_string("stdout-path", "/pl011@9000000")?;
    w.property_u64("linux,initrd-start", fdt.initrd_start)?;
    w.property_u64("linux,initrd-end", fdt.initrd_end)?;
    w.end_node(node)
}

fn memory(w: &mut FdtWriter, fdt: &GuestFdt) -> FdtWriterResult<()> {
    let node = w.begin_node(&format!("memory@{:x}", fdt.ram_base))?;
    w.property_string("device_type", "memory")?;
    w.property_array_u32("reg", &reg(fdt.ram_base, fdt.ram_size))?;
    w.end_node(node)
}

fn cpus(w: &mut FdtWriter, count: u32) -> FdtWriterResult<()> {
    let node = w.begin_node("cpus")?;
    w.property_u32("#address-cells", 1)?;
    w.property_u32("#size-cells", 0)?;
    for id in 0..count {
        let cpu = w.begin_node(&format!("cpu@{id}"))?;
        w.property_string("device_type", "cpu")?;
        w.property_string("compatible", "arm,armv8")?;
        w.property_u32("reg", id)?;
        w.property_string("enable-method", "psci")?;
        w.end_node(cpu)?;
    }
    w.end_node(node)
}

fn psci(w: &mut FdtWriter) -> FdtWriterResult<()> {
    let node = w.begin_node("psci")?;
    w.property_string_list(
        "compatible",
        vec![
            "arm,psci-1.0".to_string(),
            "arm,psci-0.2".to_string(),
            "arm,psci".to_string(),
        ],
    )?;
    w.property_string("method", "hvc")?;
    w.end_node(node)
}

fn timer(w: &mut FdtWriter) -> FdtWriterResult<()> {
    let node = w.begin_node("timer")?;
    w.property_string_list(
        "compatible",
        vec!["arm,armv8-timer".to_string(), "arm,armv7-timer".to_string()],
    )?;
    // PPI 13, 14, 11, 10, level-high. Same cells as QEMU virt `gic-version=3`.
    w.property_array_u32("interrupts", &[1, 13, 4, 1, 14, 4, 1, 11, 4, 1, 10, 4])?;
    w.property_null("always-on")?;
    w.end_node(node)
}

fn intc(w: &mut FdtWriter, phandle: u32) -> FdtWriterResult<()> {
    let node = w.begin_node(&format!("intc@{:x}", GIC_DIST_BASE))?;
    w.property_phandle(phandle)?;
    w.property_string("compatible", "arm,gic-v3")?;
    w.property_null("interrupt-controller")?;
    w.property_u32("#interrupt-cells", 3)?;
    w.property_u32("#address-cells", 2)?;
    w.property_u32("#size-cells", 2)?;
    w.property_u32("#redistributor-regions", 1)?;
    w.property_null("ranges")?;
    let mut cells = reg(GIC_DIST_BASE, GIC_DIST_SIZE).to_vec();
    cells.extend_from_slice(&reg(GIC_REDIST_BASE, GIC_REDIST_SIZE));
    w.property_array_u32("reg", &cells)?;
    w.end_node(node)
}

fn uart(w: &mut FdtWriter, clock: u32) -> FdtWriterResult<()> {
    let node = w.begin_node(&format!("pl011@{:x}", UART_BASE))?;
    w.property_string_list(
        "compatible",
        vec!["arm,pl011".to_string(), "arm,primecell".to_string()],
    )?;
    w.property_array_u32("reg", &reg(UART_BASE, PL011_REG_SIZE))?;
    w.property_array_u32("interrupts", &[0, UART_SPI, 4])?;
    w.property_array_u32("clocks", &[clock, clock])?;
    w.property_string_list(
        "clock-names",
        vec!["uartclk".to_string(), "apb_pclk".to_string()],
    )?;
    w.end_node(node)
}

fn apb_clock(w: &mut FdtWriter, phandle: u32) -> FdtWriterResult<()> {
    let node = w.begin_node("apb-pclk")?;
    w.property_phandle(phandle)?;
    w.property_string("compatible", "fixed-clock")?;
    w.property_u32("#clock-cells", 0)?;
    w.property_u32("clock-frequency", APB_HZ)?;
    w.property_string("clock-output-names", "clk24mhz")?;
    w.end_node(node)
}

fn virtio(w: &mut FdtWriter) -> FdtWriterResult<()> {
    for slot in 0..VIRTIO_MMIO_SLOTS {
        let base = VIRTIO_MMIO_BASE + slot * VIRTIO_MMIO_SLOT_SIZE;
        let node = w.begin_node(&format!("virtio_mmio@{base:x}"))?;
        w.property_string("compatible", "virtio,mmio")?;
        w.property_array_u32("reg", &reg(base, VIRTIO_MMIO_SLOT_SIZE))?;
        w.property_array_u32("interrupts", &[0, VIRTIO_SPI0 + slot as u32, 1])?;
        w.end_node(node)?;
    }
    Ok(())
}

fn reg(base: u64, size: u64) -> [u32; 4] {
    [
        (base >> 32) as u32,
        base as u32,
        (size >> 32) as u32,
        size as u32,
    ]
}

fn dump_dtb(dir: &Path, blob: &[u8]) -> Result<(), FdtError> {
    let dtb_path = dir.join("guest.dtb");
    let dts_path = dir.join("guest.dts");
    std::fs::create_dir_all(dir).map_err(|source| FdtError::Dump {
        path: dir.display().to_string(),
        source,
    })?;
    std::fs::write(&dtb_path, blob).map_err(|source| FdtError::Dump {
        path: dtb_path.display().to_string(),
        source,
    })?;
    let output = Command::new("dtc")
        .args(["-I", "dtb", "-O", "dts", "-o"])
        .arg(&dts_path)
        .arg(&dtb_path)
        .output();
    match output {
        Ok(done) if done.status.success() => {
            tracing::info!(
                target: "ternvale::boot",
                path = %dts_path.display(),
                bytes = blob.len(),
                "decompiled guest dtb"
            );
        }
        Ok(done) => {
            tracing::warn!(
                target: "ternvale::boot",
                status = %done.status,
                stderr = %String::from_utf8_lossy(&done.stderr),
                "dtc did not decompile the guest dtb"
            );
        }
        Err(error) => {
            tracing::warn!(
                target: "ternvale::boot",
                error = %error,
                "dtc is not available; wrote guest.dtb only"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "fdt_tests.rs"]
mod tests;
