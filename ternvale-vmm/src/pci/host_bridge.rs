//! Host bridge at 00:00.0: identity only, no BARs, no interrupts.

use super::{ConfigSpace, Header, PciFunction};

/// Red Hat (QEMU) vendor ID, the same IDs QEMU's `gpex` host bridge uses.
/// Linux and EDK2 only need class 0x0600 here; the IDs keep `lspci` familiar.
pub const HOST_BRIDGE_VENDOR_ID: u16 = 0x1b36;
/// QEMU "PCIe host bridge" device ID.
pub const HOST_BRIDGE_DEVICE_ID: u16 = 0x0008;
const CLASS_HOST_BRIDGE: u32 = 0x06_00_00;

/// The root complex's own function on device 0.
pub struct HostBridge {
    config: ConfigSpace,
}

impl HostBridge {
    /// Build the host bridge header.
    #[tracing::instrument(level = "debug", target = "ternvale::pci", skip_all)]
    pub fn new() -> Self {
        tracing::debug!(
            target: "ternvale::pci",
            vendor = format!("{HOST_BRIDGE_VENDOR_ID:#06x}"),
            device = format!("{HOST_BRIDGE_DEVICE_ID:#06x}"),
            "pci host bridge created"
        );
        Self {
            config: ConfigSpace::new(Header {
                vendor_id: HOST_BRIDGE_VENDOR_ID,
                device_id: HOST_BRIDGE_DEVICE_ID,
                class_code: CLASS_HOST_BRIDGE,
                revision: 0,
                subsystem_vendor_id: HOST_BRIDGE_VENDOR_ID,
                subsystem_id: 0x1100,
                interrupt_pin: 0,
            }),
        }
    }
}

impl Default for HostBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl PciFunction for HostBridge {
    fn name(&self) -> &str {
        "pci-host-bridge"
    }

    fn config(&self) -> &ConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut ConfigSpace {
        &mut self.config
    }
}
