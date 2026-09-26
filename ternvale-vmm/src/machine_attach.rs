//! Extra MMIO devices attached after UART and GIC redistributor.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::memory::GuestMemory;
use crate::mmio::MmioDevice;
use crate::serial::SerialDevice;

/// One SPI that the poll loop should re-assert while the line stays high.
pub type SpiLevel = (u32, Arc<AtomicBool>);
/// Shared list of active SPI level flags.
pub type SpiLevels = Arc<Mutex<Vec<SpiLevel>>>;

/// Guest memory and GIC available while building MMIO devices.
pub struct DeviceAttach {
    /// Shared guest RAM. Virtio workers lock it around descriptor I/O.
    pub memory: Arc<Mutex<GuestMemory>>,
    /// Interrupt controller for SPI injection.
    pub gic: Arc<ternvale_hv::Gic>,
    /// Virtio (and other) SPI lines the poll loop re-pulses during WFI.
    pub spi_levels: SpiLevels,
}

impl DeviceAttach {
    /// Hook that drives virtio-mmio slot `slot` through `hv_gic_set_spi`.
    ///
    /// The FDT SPI number is `0x10 + slot`. The Hypervisor intid is that value
    /// plus 32, matching the UART wiring.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip(self), fields(slot))]
    pub fn virtio_irq_hook(&self, slot: u32) -> Arc<dyn Fn(bool) + Send + Sync> {
        let spi = 32 + crate::fdt::VIRTIO_SPI0 + slot;
        let gic = Arc::clone(&self.gic);
        let level = Arc::new(AtomicBool::new(false));
        match self.spi_levels.lock() {
            Ok(mut list) => list.push((spi, Arc::clone(&level))),
            Err(poison) => poison.into_inner().push((spi, Arc::clone(&level))),
        }
        Arc::new(move |assert| {
            if level.swap(assert, Ordering::AcqRel) == assert {
                return;
            }
            if let Err(error) = gic.set_spi(spi, assert) {
                tracing::warn!(
                    target: "ternvale::gic",
                    irq = spi,
                    level = assert,
                    error = %error,
                    "virtio spi update failed"
                );
            }
        })
    }
}

pub(super) fn install_uart_irq(
    serial: Rc<std::cell::RefCell<Box<dyn SerialDevice>>>,
    gic: Arc<ternvale_hv::Gic>,
    level_flag: Arc<AtomicBool>,
) {
    let spi = 32 + crate::fdt::UART_SPI;
    serial.borrow_mut().set_irq_hook(Arc::new(move |level| {
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

pub(super) struct LocalUart(pub(super) Rc<std::cell::RefCell<Box<dyn SerialDevice>>>);

impl MmioDevice for LocalUart {
    fn name(&self) -> &str {
        "pl011"
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        self.0.borrow_mut().read(offset, size)
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        self.0.borrow_mut().write(offset, size, val);
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
