//! Shared root complex state plus its two MMIO bus windows: ECAM and BARs.

use std::sync::{Arc, Mutex};

use super::bus::{all_ones, PciBus};
use super::{
    register_name, swizzle, Bdf, HostBridge, IntxLine, IntxPin, LineHook, PciError, PciFunction,
    PCI_INTX_LINES, PCI_INTX_SPI0,
};
use crate::lockwatch::Guard;
use crate::mmio::{MmioBus, MmioDevice};
use crate::platform::{PCIE_ECAM_BASE, PCIE_ECAM_SIZE, PCIE_MMIO_BASE, PCIE_MMIO_SIZE};

/// Buses the ECAM window covers (1 MiB each). Only bus 0 has functions.
pub const ECAM_BUSES: u8 = (PCIE_ECAM_SIZE >> 20) as u8;

/// Bus 0, the INTx lines, and the host bridge at 00:00.0.
pub struct PciRoot {
    bus: Mutex<PciBus>,
    lines: Vec<Arc<IntxLine>>,
}

impl PciRoot {
    /// Root complex whose INTx line `i` drives FDT SPI `PCI_INTX_SPI0 + i`
    /// through `hooks[i]`.
    #[tracing::instrument(level = "debug", target = "ternvale::pci", skip_all)]
    pub fn new(hooks: [LineHook; PCI_INTX_LINES]) -> Arc<Self> {
        let lines = hooks
            .into_iter()
            .enumerate()
            .map(|(i, hook)| IntxLine::new(PCI_INTX_SPI0 + i as u32, hook))
            .collect();
        let mut bus = PciBus::new(PCIE_MMIO_BASE, PCIE_MMIO_SIZE);
        bus.insert(0, Box::new(HostBridge::new()), None);
        tracing::info!(
            target: "ternvale::pci",
            ecam = format!("{PCIE_ECAM_BASE:#x}"),
            buses = ECAM_BUSES,
            window = format!("{PCIE_MMIO_BASE:#x}"),
            window_size = format!("{PCIE_MMIO_SIZE:#x}"),
            "pci root complex created"
        );
        Arc::new(Self {
            bus: Mutex::new(bus),
            lines,
        })
    }

    fn lock(&self) -> Guard<'_, PciBus> {
        crate::lockwatch::lock(&self.bus, "pci-bus")
    }

    /// Put a function on the lowest free device number. `build` gets its
    /// address and its INTA pin (already swizzled onto a line).
    #[tracing::instrument(level = "debug", target = "ternvale::pci", skip_all)]
    pub fn add<F>(&self, build: F) -> Result<Bdf, PciError>
    where
        F: FnOnce(Bdf, Arc<IntxPin>) -> Result<Box<dyn PciFunction>, String>,
    {
        let mut bus = self.lock();
        let device = bus.free_device().ok_or_else(|| {
            tracing::warn!(target: "ternvale::pci", "pci bus 0 is full");
            PciError::BusFull
        })?;
        let bdf = Bdf::new(device);
        let line = Arc::clone(&self.lines[swizzle(device, 1)]);
        let spi = line.spi();
        let pin = IntxPin::new(format!("pci-{bdf}"), line);
        let function = build(bdf, Arc::clone(&pin)).map_err(|reason| {
            tracing::warn!(target: "ternvale::pci", %bdf, reason = %reason, "pci function build failed");
            PciError::Build { bdf, reason }
        })?;
        let caps = function.config().capabilities().inspect_err(|error| {
            tracing::error!(target: "ternvale::pci", %bdf, function = function.name(), error = %error, "pci function has a malformed capability list");
        })?;
        for cap in &caps {
            tracing::debug!(
                target: "ternvale::pci",
                %bdf,
                offset = format!("{:#x}", cap.offset),
                id = format!("{:#04x}", cap.id),
                next = format!("{:#x}", cap.next),
                "pci capability"
            );
        }
        tracing::info!(
            target: "ternvale::pci",
            %bdf,
            function = function.name(),
            vendor = format!("{:#06x}", function.config().u16_at(0)),
            device_id = format!("{:#06x}", function.config().u16_at(2)),
            intx_spi = spi,
            capabilities = caps.len(),
            "pci function added"
        );
        bus.insert(device, function, Some(pin));
        Ok(bdf)
    }

    /// Host BAR pre-assignment; returns how many BARs got an address.
    #[tracing::instrument(level = "debug", target = "ternvale::pci", skip_all)]
    pub fn assign_bars(&self) -> usize {
        self.lock().assign_bars()
    }

    /// Assign BARs, then map the ECAM and BAR windows onto `bus`.
    #[tracing::instrument(level = "debug", target = "ternvale::pci", skip_all)]
    pub fn register(self: &Arc<Self>, bus: &mut MmioBus) -> Result<(), PciError> {
        let assigned = self.assign_bars();
        bus.register(PCIE_ECAM_BASE, PCIE_ECAM_SIZE, Box::new(PciEcam(Arc::clone(self))))
            .inspect_err(|error| tracing::error!(target: "ternvale::pci", error = %error, "register pci ecam window failed"))?;
        bus.register(PCIE_MMIO_BASE, PCIE_MMIO_SIZE, Box::new(PciWindow(Arc::clone(self))))
            .inspect_err(|error| tracing::error!(target: "ternvale::pci", error = %error, "register pci mmio window failed"))?;
        let functions = self.lock().functions();
        for (bdf, name) in &functions {
            tracing::debug!(target: "ternvale::pci", %bdf, function = %name, "pci function present");
        }
        tracing::info!(
            target: "ternvale::pci",
            functions = functions.len(),
            bars_assigned = assigned,
            "pci host bridge registered"
        );
        Ok(())
    }

    /// Populated functions on bus 0, including the host bridge.
    pub fn function_count(&self) -> usize {
        self.lock().functions().len()
    }

    /// Config read of `bdf` register `reg`, as the ECAM window does it.
    pub fn config_read(&self, bdf: Bdf, reg: u16, size: u8) -> u32 {
        self.lock()
            .config_read(bdf, reg, size)
            .unwrap_or_else(|| all_ones(size))
    }

    /// Config write of `bdf` register `reg`, as the ECAM window does it.
    /// Returns the register value after the write masks, `None` if absent.
    pub fn config_write(&self, bdf: Bdf, reg: u16, size: u8, value: u32) -> Option<u32> {
        self.lock().config_write(bdf, reg, size, value)
    }
}

/// `offset` inside the ECAM window to (function, register), if well formed.
fn ecam_decode(offset: u64, size: u8) -> Result<(Bdf, u16), &'static str> {
    if !matches!(size, 1 | 2 | 4) {
        return Err("ecam access is not 1, 2, or 4 bytes");
    }
    if offset % u64::from(size) != 0 {
        return Err("ecam access is not naturally aligned");
    }
    let bdf = Bdf {
        bus: (offset >> 20) as u8,
        device: ((offset >> 15) & 0x1f) as u8,
        function: ((offset >> 12) & 7) as u8,
    };
    Ok((bdf, (offset & 0xfff) as u16))
}

/// ECAM config window at `PCIE_ECAM_BASE`.
struct PciEcam(Arc<PciRoot>);

impl PciEcam {
    fn decode(&self, offset: u64, size: u8, direction: &'static str) -> Option<(Bdf, u16)> {
        ecam_decode(offset, size)
            .inspect_err(|reason| {
                tracing::warn!(
                    target: "ternvale::pci",
                    offset = format!("{offset:#x}"),
                    size,
                    direction,
                    reason,
                    "rejected ecam access"
                );
            })
            .ok()
    }
}

impl MmioDevice for PciEcam {
    fn name(&self) -> &str {
        "pci-ecam"
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        let Some((bdf, reg)) = self.decode(offset, size, "read") else {
            return u64::from(all_ones(size.min(4)));
        };
        let value = self.0.lock().config_read(bdf, reg, size);
        tracing::trace!(
            target: "ternvale::pci",
            addr = format!("{:#x}", PCIE_ECAM_BASE + offset),
            %bdf,
            reg = format!("{reg:#x}"),
            field = register_name(reg),
            size,
            value = format!("{:#x}", value.unwrap_or_else(|| all_ones(size))),
            present = value.is_some(),
            direction = "read",
            "pci config"
        );
        u64::from(value.unwrap_or_else(|| all_ones(size)))
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        let Some((bdf, reg)) = self.decode(offset, size, "write") else {
            return;
        };
        let stored = self.0.config_write(bdf, reg, size, val as u32);
        tracing::trace!(
            target: "ternvale::pci",
            addr = format!("{:#x}", PCIE_ECAM_BASE + offset),
            %bdf,
            reg = format!("{reg:#x}"),
            field = register_name(reg),
            size,
            value = format!("{val:#x}"),
            stored = ?stored.map(|v| format!("{v:#x}")),
            present = stored.is_some(),
            direction = "write",
            "pci config"
        );
    }
}

/// The 32-bit BAR window at `PCIE_MMIO_BASE`, routed by programmed BARs.
struct PciWindow(Arc<PciRoot>);

impl MmioDevice for PciWindow {
    fn name(&self) -> &str {
        "pci-mmio"
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        let gpa = PCIE_MMIO_BASE + offset;
        let value = self.0.lock().mmio_read(gpa, size);
        tracing::trace!(target: "ternvale::pci", gpa = format!("{gpa:#x}"), size, value = ?value.map(|v| format!("{v:#x}")), direction = "read", "pci bar access");
        value.unwrap_or_else(|| {
            tracing::warn!(target: "ternvale::pci", gpa = format!("{gpa:#x}"), size, "read of pci window with no decoding bar");
            u64::MAX
        })
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        let gpa = PCIE_MMIO_BASE + offset;
        tracing::trace!(target: "ternvale::pci", gpa = format!("{gpa:#x}"), size, value = format!("{val:#x}"), direction = "write", "pci bar access");
        if !self.0.lock().mmio_write(gpa, size, val) {
            tracing::warn!(target: "ternvale::pci", gpa = format!("{gpa:#x}"), size, value = format!("{val:#x}"), "write to pci window with no decoding bar");
        }
    }
}
