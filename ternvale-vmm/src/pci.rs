//! PCIe generic ECAM host bridge (`pci-host-ecam-generic`), bus 0 only.
//!
//! Config space is reached through the ECAM window at [`PCIE_ECAM_BASE`]
//! (`bus << 20 | device << 15 | function << 12 | register`). Memory BARs decode
//! inside the 32-bit window at [`PCIE_MMIO_BASE`], which the DTB `ranges`
//! property maps 1:1. The MMIO bus is fixed once vCPUs run, so one bus device
//! covers the whole window and routes each access by the BAR values the guest
//! programmed. INTx pins swizzle onto four level SPIs, as on QEMU virt. MSI-X
//! is a logged stub (see `docs/ARCHITECTURE.md`).
//!
//! Lock order: the MMIO bus slot (`pci-ecam` / `pci-mmio`), then `pci-bus`,
//! then `pci-intx-pin`, then `pci-intx-line`. Device workers only take the
//! last two.
//!
//! [`PCIE_ECAM_BASE`]: crate::platform::PCIE_ECAM_BASE
//! [`PCIE_MMIO_BASE`]: crate::platform::PCIE_MMIO_BASE

mod bus;
mod capability;
mod config;
mod host_bridge;
mod intx;
mod msix;
mod root;

pub use capability::{Capability, CAP_SPACE_START};
pub use config::{
    register_name, Bar, BarKind, ConfigSpace, Header, CAP_PTR, COMMAND, COMMAND_INTX_DISABLE,
    COMMAND_MASTER, COMMAND_MEMORY, CONFIG_SPACE_SIZE, INTERRUPT_LINE, INTERRUPT_PIN, STATUS,
    STATUS_CAP_LIST, STATUS_INTERRUPT,
};
pub use host_bridge::{HostBridge, HOST_BRIDGE_DEVICE_ID, HOST_BRIDGE_VENDOR_ID};
pub use intx::{swizzle, IntxLine, IntxPin, LineHook, PCI_INTX_LINES, PCI_INTX_SPI0};
pub use msix::{Msix, MSIX_CAP_ID, MSIX_ENTRY_SIZE, MSIX_NO_VECTOR};
pub use root::{PciRoot, ECAM_BUSES};

/// Bus, device, and function of one PCI function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Bdf {
    /// Bus number. Only bus 0 is populated.
    pub bus: u8,
    /// Device (slot) number, 0..32.
    pub device: u8,
    /// Function number, 0..8.
    pub function: u8,
}

impl Bdf {
    /// Function 0 of `device` on bus 0.
    pub const fn new(device: u8) -> Self {
        Self {
            bus: 0,
            device,
            function: 0,
        }
    }

    /// `device << 3 | function`, the low byte of the DT `interrupt-map` unit address.
    pub const fn devfn(self) -> u8 {
        (self.device << 3) | (self.function & 7)
    }
}

impl std::fmt::Display for Bdf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02x}:{:02x}.{}", self.bus, self.device, self.function)
    }
}

/// Building the PCI topology failed.
#[derive(Debug, thiserror::Error)]
pub enum PciError {
    /// Every device number on bus 0 is taken.
    #[error("pci bus 0 has no free device number")]
    BusFull,
    /// A BAR index is outside 0..6, or a 64-bit BAR would start at index 5.
    #[error("pci bar index {index} is invalid for a {kind:?} bar")]
    BarIndex {
        /// Requested BAR index.
        index: usize,
        /// Requested BAR kind.
        kind: BarKind,
    },
    /// A BAR size is not a power of two, is below 16 bytes, or is too large.
    #[error("pci bar size {size:#x} is invalid for a {kind:?} bar")]
    BarSize {
        /// Requested size.
        size: u64,
        /// Requested BAR kind.
        kind: BarKind,
    },
    /// The BAR register (or the upper half of a 64-bit BAR) is already in use.
    #[error("pci bar {index} is already in use")]
    BarTaken {
        /// Requested BAR index.
        index: usize,
    },
    /// The capability does not fit in the 256-byte config space.
    #[error("pci capability {id:#x} ({len} bytes) does not fit in config space")]
    CapabilitySpace {
        /// Capability ID.
        id: u8,
        /// Capability length in bytes.
        len: usize,
    },
    /// The capability list is malformed (a loop or a pointer into the header).
    #[error("pci capability list is malformed at {offset:#x}: {reason}")]
    CapabilityList {
        /// Offending pointer.
        offset: u8,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The device builder failed.
    #[error("build pci function {bdf}: {reason}")]
    Build {
        /// Function being built.
        bdf: Bdf,
        /// Builder error text.
        reason: String,
    },
    /// The MMIO bus rejected the ECAM or BAR window.
    #[error("register pci window: {0}")]
    Mmio(#[from] crate::mmio::MmioError),
}

/// One PCI function on bus 0.
///
/// The default config accessors go straight to [`ConfigSpace`], which applies
/// the per-byte write masks (BAR sizing, command register, capabilities).
/// Functions override them only for dynamic fields such as virtio `pci_cfg`.
pub trait PciFunction: Send {
    /// Short name for logs, for example `virtio-pci-00:01.0`.
    fn name(&self) -> &str;

    /// Config space header, BARs, and capabilities.
    fn config(&self) -> &ConfigSpace;

    /// Mutable config space, used by the bus for host BAR assignment.
    fn config_mut(&mut self) -> &mut ConfigSpace;

    /// Guest config read. `offset` is aligned to `size` and below 4096.
    fn read_config(&mut self, offset: u16, size: u8) -> u32 {
        self.config().read(offset, size)
    }

    /// Guest config write. `offset` is aligned to `size` and below 4096.
    fn write_config(&mut self, offset: u16, size: u8, value: u32) {
        self.config_mut().write(offset, size, value);
    }

    /// Guest read at `offset` inside memory BAR `bar`.
    fn read_bar(&mut self, bar: usize, offset: u64, size: u8) -> u64 {
        tracing::warn!(
            target: "ternvale::pci",
            function = self.name(),
            bar,
            offset = format!("{offset:#x}"),
            size,
            "read of a bar this function does not implement"
        );
        u64::MAX
    }

    /// Guest write at `offset` inside memory BAR `bar`.
    fn write_bar(&mut self, bar: usize, offset: u64, size: u8, value: u64) {
        tracing::warn!(
            target: "ternvale::pci",
            function = self.name(),
            bar,
            offset = format!("{offset:#x}"),
            size,
            value = format!("{value:#x}"),
            "write to a bar this function does not implement"
        );
    }
}

#[cfg(test)]
#[path = "pci_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "pci_bus_tests.rs"]
mod bus_tests;

#[cfg(test)]
#[path = "pci_config_tests.rs"]
mod config_tests;
