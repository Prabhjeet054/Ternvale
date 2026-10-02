//! Extra MMIO devices attached after UART and GIC redistributor.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::lockwatch::Guard;
use crate::memory::GuestMemory;
use crate::mmio::MmioDevice;
use crate::pci::{Bdf, IntxPin, LineHook, PciFunction, PciRoot, PCI_INTX_LINES, PCI_INTX_SPI0};
use crate::serial::SerialDevice;

/// One SPI that the poll loop should re-assert while the line stays high.
pub type SpiLevel = (u32, Arc<AtomicBool>);
/// Shared list of active SPI level flags.
pub type SpiLevels = Arc<Mutex<Vec<SpiLevel>>>;
/// The UART, shared by the bus (every vCPU) and the stdin thread.
pub(super) type SharedSerial = Arc<Mutex<Box<dyn SerialDevice>>>;

pub(super) fn lock_serial(serial: &SharedSerial) -> Guard<'_, Box<dyn SerialDevice>> {
    crate::lockwatch::lock(serial, "serial")
}

/// Guest memory, GIC, and PCI root available while building devices.
pub struct DeviceAttach {
    /// Shared guest RAM. Virtio workers lock it around descriptor I/O.
    pub memory: Arc<Mutex<GuestMemory>>,
    /// Interrupt controller for SPI injection.
    pub gic: Arc<ternvale_hv::Gic>,
    /// Virtio (and other) SPI lines the poll loop re-pulses during WFI.
    pub spi_levels: SpiLevels,
    /// PCIe root complex. Functions added here appear behind the ECAM window.
    pub pci: Arc<PciRoot>,
}

impl DeviceAttach {
    /// Attach context whose PCI INTx lines drive SPIs through [`Self::spi_hook`].
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
    pub fn new(
        memory: Arc<Mutex<GuestMemory>>,
        gic: Arc<ternvale_hv::Gic>,
        spi_levels: SpiLevels,
    ) -> Self {
        let hooks: [LineHook; PCI_INTX_LINES] =
            std::array::from_fn(|i| spi_hook(&gic, &spi_levels, PCI_INTX_SPI0 + i as u32));
        Self {
            memory,
            gic,
            spi_levels,
            pci: PciRoot::new(hooks),
        }
    }

    /// Hook that drives FDT SPI `spi` (intid `32 + spi`) through
    /// `hv_gic_set_spi`, only on level changes. The poll loop re-pulses it
    /// while it stays high.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip(self), fields(spi))]
    pub fn spi_hook(&self, spi: u32) -> Arc<dyn Fn(bool) + Send + Sync> {
        spi_hook(&self.gic, &self.spi_levels, spi)
    }

    /// Hook that drives virtio-mmio slot `slot`: FDT SPI `0x10 + slot`.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip(self), fields(slot))]
    pub fn virtio_irq_hook(&self, slot: u32) -> Arc<dyn Fn(bool) + Send + Sync> {
        self.spi_hook(crate::fdt::VIRTIO_SPI0 + slot)
    }

    /// Add a PCI function on the next free device number of bus 0.
    #[tracing::instrument(level = "debug", target = "ternvale::pci", skip_all)]
    pub fn add_pci<F>(&self, build: F) -> Result<Bdf, crate::machine::MachineError>
    where
        F: FnOnce(Bdf, Arc<IntxPin>) -> Result<Box<dyn PciFunction>, String>,
    {
        self.pci
            .add(build)
            .map_err(crate::machine::MachineError::from)
    }
}

fn spi_hook(
    gic: &Arc<ternvale_hv::Gic>,
    spi_levels: &SpiLevels,
    spi: u32,
) -> Arc<dyn Fn(bool) + Send + Sync> {
    let intid = 32 + spi;
    let gic = Arc::clone(gic);
    let level = Arc::new(AtomicBool::new(false));
    crate::lockwatch::lock(spi_levels, "spi-levels").push((intid, Arc::clone(&level)));
    Arc::new(move |assert| {
        if level.swap(assert, Ordering::AcqRel) == assert {
            return;
        }
        if let Err(error) = gic.set_spi(intid, assert) {
            tracing::warn!(
                target: "ternvale::gic",
                irq = intid,
                level = assert,
                error = %error,
                "device spi update failed"
            );
        }
    })
}

pub(super) fn install_uart_irq(
    serial: &SharedSerial,
    gic: Arc<ternvale_hv::Gic>,
    level_flag: Arc<AtomicBool>,
) {
    let spi = 32 + crate::fdt::UART_SPI;
    lock_serial(serial).set_irq_hook(Arc::new(move |level| {
        // set_spi(true) also pulses an edge. Repeat calls while the line stays
        // high leave a pending SPI after the PL011 has cleared MIS.
        if level_flag.swap(level, Ordering::AcqRel) == level {
            return;
        }
        if let Err(error) = gic.set_spi(spi, level) {
            tracing::warn!(
                target: "ternvale::gic",
                irq = spi,
                level,
                error = %error,
                "uart spi update failed"
            );
        }
    }));
}

/// Bus window for the UART. Lock order: the bus slot lock, then the serial lock.
pub(super) struct SharedUart(pub(super) SharedSerial);

impl MmioDevice for SharedUart {
    fn name(&self) -> &str {
        "pl011"
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        lock_serial(&self.0).read(offset, size)
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        lock_serial(&self.0).write(offset, size, val);
    }
}

pub(super) fn read_boot_file(
    what: &'static str,
    path: &std::path::Path,
) -> Result<Vec<u8>, crate::machine::MachineError> {
    std::fs::read(path).map_err(|source| {
        tracing::error!(
            target: "ternvale::boot",
            what,
            path = %path.display(),
            error = %source,
            "failed to read boot file"
        );
        crate::machine::MachineError::Read {
            what,
            path: path.to_path_buf(),
            source,
        }
    })
}
