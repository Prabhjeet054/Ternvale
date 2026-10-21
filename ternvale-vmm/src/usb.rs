//! Virtual xHCI at [`crate::platform::XHCI_BASE`] on the MMIO bus.

use std::sync::{Arc, Mutex};

use ternvale_config::VmConfig;
use ternvale_usb::{
    attach_shared_with_passthrough, GuestMemoryAccess, MemError, PassthroughBinding, UsbInput,
    Xhci, MMIO_SIZE,
};
use ternvale_usb_host::claim_devices;

use crate::machine::MachineError;
use crate::memory::GuestMemory;
use crate::mmio::{MmioBus, MmioDevice};
use crate::platform::{XHCI_BASE, XHCI_SIZE};
use crate::DeviceAttach;

/// Name in MMIO logs and the shutdown device counters.
pub const XHCI_DEVICE_NAME: &str = "xhci";

const _: () = assert!(MMIO_SIZE <= XHCI_SIZE);

/// DTB / ACPI SPI index for the xHCI interrupt line (GIC INTID `32 + USB_SPI`).
pub const USB_SPI: u32 = 7;

struct GuestMemAdapter(Arc<Mutex<GuestMemory>>);

impl GuestMemoryAccess for GuestMemAdapter {
    fn read_bytes(&self, gpa: u64, buf: &mut [u8]) -> Result<(), MemError> {
        self.0
            .lock()
            .map_err(|_| MemError::Read {
                gpa,
                message: "guest memory lock poisoned".to_string(),
            })?
            .read_bytes(gpa, buf)
            .map_err(|source| MemError::Read {
                gpa,
                message: source.to_string(),
            })
    }

    fn write_bytes(&mut self, gpa: u64, data: &[u8]) -> Result<(), MemError> {
        self.0
            .lock()
            .map_err(|_| MemError::Write {
                gpa,
                message: "guest memory lock poisoned".to_string(),
            })?
            .write_bytes(gpa, data)
            .map_err(|source| MemError::Write {
                gpa,
                message: source.to_string(),
            })
    }
}

/// MMIO wrapper around [`Xhci`].
pub struct XhciDevice {
    inner: Arc<Mutex<Xhci>>,
}

impl MmioDevice for XhciDevice {
    fn name(&self) -> &str {
        XHCI_DEVICE_NAME
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        self.inner.lock().map(|c| c.read(offset, size)).unwrap_or(0)
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.write(offset, size, val);
        }
    }
}

/// Put the xHCI controller on `bus` when `[usb] enabled` is true (default).
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all, fields(vm = %config.name))]
pub(crate) fn register(
    bus: &mut MmioBus,
    memory: Arc<Mutex<GuestMemory>>,
    attach: &DeviceAttach,
    config: &VmConfig,
) -> Result<Option<UsbInput>, MachineError> {
    if !config.usb_enabled() {
        tracing::info!(target: "ternvale::usb", "usb disabled ([usb] enabled = false): no xhci device");
        return Ok(None);
    }
    let adapter: Arc<Mutex<dyn GuestMemoryAccess + Send>> =
        Arc::new(Mutex::new(GuestMemAdapter(Arc::clone(&memory))));
    let irq = attach.spi_hook(USB_SPI);
    let usb_cfg = config.usb_section();
    let port_entries = usb_cfg.passthrough_ports();
    let claimed = claim_devices(&usb_cfg.passthrough);
    let bindings: Vec<PassthroughBinding> = claimed
        .into_iter()
        .map(|(entry, backend)| {
            let port = port_entries
                .iter()
                .find(|(_, vid, pid)| *vid == entry.vendor_id && *pid == entry.product_id)
                .map(|(port, _, _)| *port)
                .unwrap_or(3);
            PassthroughBinding { port, backend }
        })
        .collect();
    let (inner, input) =
        attach_shared_with_passthrough(XHCI_BASE, adapter, irq, &port_entries, bindings)
            .map_err(|error| MachineError::Attach(format!("xhci: {error}")))?;
    bus.register(XHCI_BASE, MMIO_SIZE, Box::new(XhciDevice { inner }))?;
    tracing::info!(
        target: "ternvale::usb",
        base = format!("{XHCI_BASE:#x}"),
        size = format!("{MMIO_SIZE:#x}"),
        spi = USB_SPI,
        "xhci registered on the mmio bus"
    );
    Ok(Some(input))
}
