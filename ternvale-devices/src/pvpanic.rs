//! PCI `pvpanic` (Red Hat/QEMU `1b36:0011`) for virtio-win and Linux `pvpanic-pci`.
//!
//! TODO(verify): virtio-win ARM64 still ships `pvpanic.sys` for this ID
//! (`https://github.com/virtio-win/kvm-guest-drivers-windows` `pvpanic`).

use std::sync::Arc;

use ternvale_vmm::crash::{
    CrashHub, CrashKind, PVPANIC_EVENTS, PVPANIC_PCI_DEVICE, PVPANIC_PCI_VENDOR,
};
use ternvale_vmm::pci::{Bar, BarKind, Bdf, ConfigSpace, Header, IntxPin, PciFunction};
use ternvale_vmm::{DeviceAttach, MachineError};

/// BAR 0 size (QEMU uses a 16-byte memory BAR).
const BAR_SIZE: u64 = 16;

/// PCI function whose BAR 0 is the pvpanic event byte.
pub struct PvPanicPci {
    name: String,
    config: ConfigSpace,
    hub: Arc<CrashHub>,
}

impl PvPanicPci {
    /// Next free slot on `attach.pci`.
    #[tracing::instrument(level = "debug", target = "ternvale::pvpanic", skip_all)]
    pub fn attach(attach: &DeviceAttach, hub: Arc<CrashHub>) -> Result<Bdf, MachineError> {
        let hub = Arc::clone(&hub);
        attach.add_pci(move |bdf, _pin: Arc<IntxPin>| {
            Self::function(bdf, hub).map(|f| Box::new(f) as Box<dyn PciFunction>)
        })
    }

    #[tracing::instrument(level = "debug", target = "ternvale::pvpanic", skip_all, fields(%bdf))]
    fn function(bdf: Bdf, hub: Arc<CrashHub>) -> Result<Self, String> {
        let mut config = ConfigSpace::new(Header {
            vendor_id: PVPANIC_PCI_VENDOR,
            device_id: PVPANIC_PCI_DEVICE,
            class_code: 0x08_80_00,
            revision: 1,
            subsystem_vendor_id: PVPANIC_PCI_VENDOR,
            subsystem_id: PVPANIC_PCI_DEVICE,
            interrupt_pin: 1,
        });
        config
            .add_bar(
                0,
                Bar {
                    size: BAR_SIZE,
                    kind: BarKind::Mem32,
                    prefetchable: false,
                },
            )
            .map_err(|error| error.to_string())?;
        tracing::info!(
            target: "ternvale::pvpanic",
            %bdf,
            vendor = format!("{PVPANIC_PCI_VENDOR:#06x}"),
            device = format!("{PVPANIC_PCI_DEVICE:#06x}"),
            "pvpanic-pci attached"
        );
        Ok(Self {
            name: format!("pvpanic-pci-{bdf}"),
            config,
            hub,
        })
    }
}

impl PciFunction for PvPanicPci {
    fn name(&self) -> &str {
        &self.name
    }

    fn config(&self) -> &ConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut ConfigSpace {
        &mut self.config
    }

    fn read_bar(&mut self, bar: usize, offset: u64, size: u8) -> u64 {
        let _ = (bar, size);
        if offset > 1 {
            return 0;
        }
        u64::from(PVPANIC_EVENTS)
    }

    fn write_bar(&mut self, bar: usize, offset: u64, size: u8, value: u64) {
        let _ = (bar, size);
        if offset > 1 {
            return;
        }
        let events = (value as u8) & PVPANIC_EVENTS;
        if events != 0 {
            self.hub.report(CrashKind::Panic { events });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternvale_vmm::crash::PVPANIC_PANICKED;

    #[test]
    fn pci_ids_match_qemu_and_bar0_advertises_event_bits() {
        let hub = CrashHub::new();
        let mut dev = PvPanicPci::function(Bdf::new(4), hub).expect("fn");
        assert_eq!(dev.config().u16_at(0), PVPANIC_PCI_VENDOR);
        assert_eq!(dev.config().u16_at(2), PVPANIC_PCI_DEVICE);
        assert_eq!(dev.read_bar(0, 0, 1) as u8, PVPANIC_EVENTS);
        dev.write_bar(0, 0, 1, u64::from(PVPANIC_PANICKED));
    }
}
