//! What a guest's PCI enumeration learns about one function, in the notations
//! of the logs it can be matched against.
//!
//! - Windows hardware and compatible IDs: Microsoft Learn, "Identifiers for
//!   PCI Devices" (windows-hardware/drivers/install/identifiers-for-pci-devices,
//!   read 2026-10-15): `PCI\VEN_v(4)&DEV_d(4)&SUBSYS_s(4)n(4)&REV_r(2)`, where
//!   `s(4)` is the subsystem ID and `n(4)` the subsystem vendor ID, and the
//!   class-code compatible ID `PCI\CC_c(2)s(2)p(2)`. Driver INF files match
//!   against these strings: virtio-win 0.1.302's ARM64 `viostor.inf`
//!   (`[VioStor.NTARM64.10.0...16299]`) lists
//!   `PCI\VEN_1AF4&DEV_1042&SUBSYS_11001AF4&REV_01`, which is what a Ternvale
//!   virtio-blk PCI function reports (checked by `scripts/fetch-virtio-win.sh`).
//! - EDK2: `PciBusDxe` logs every function it finds at `DEBUG_INFO`
//!   (`MdeModulePkg/Bus/Pci/PciBusDxe/PciEnumeratorSupport.c`) as
//!   `PciBus: Discovered PCI @ [bus|dev|fn]  [VID = 0x...., DID = 0x....]`,
//!   seen verbatim in the debug ArmVirtQemu serial log. EDK2's `PrintLib`
//!   prints `%x` in upper case.

use std::fmt;

use super::{Bdf, PciRoot};

/// Identity registers of a type 0 header (PCI Local Bus 3.0, 6.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciIdentity {
    /// Bus address.
    pub bdf: Bdf,
    /// Vendor ID (offset 0x00).
    pub vendor_id: u16,
    /// Device ID (offset 0x02).
    pub device_id: u16,
    /// Revision ID (offset 0x08).
    pub revision: u8,
    /// Class code, base class in bits 23:16 (offsets 0x09..=0x0b).
    pub class_code: u32,
    /// Subsystem vendor ID (offset 0x2c).
    pub subsystem_vendor_id: u16,
    /// Subsystem ID (offset 0x2e).
    pub subsystem_id: u16,
}

impl PciIdentity {
    /// Read `bdf`'s identity through config space, as the guest does. `None`
    /// when no function answers (vendor ID all ones).
    #[tracing::instrument(level = "debug", target = "ternvale::pci", skip_all, fields(%bdf))]
    pub fn read(root: &PciRoot, bdf: Bdf) -> Option<Self> {
        let vendor_id = root.config_read(bdf, 0x00, 2) as u16;
        if vendor_id == u16::MAX {
            tracing::debug!(target: "ternvale::pci", %bdf, "no function at this address");
            return None;
        }
        Some(Self {
            bdf,
            vendor_id,
            device_id: root.config_read(bdf, 0x02, 2) as u16,
            revision: root.config_read(bdf, 0x08, 1) as u8,
            class_code: root.config_read(bdf, 0x08, 4) >> 8,
            subsystem_vendor_id: root.config_read(bdf, 0x2c, 2) as u16,
            subsystem_id: root.config_read(bdf, 0x2e, 2) as u16,
        })
    }

    /// The most specific Windows hardware ID, e.g.
    /// `PCI\VEN_1AF4&DEV_1042&SUBSYS_11001AF4&REV_01`.
    #[tracing::instrument(level = "trace", target = "ternvale::pci", skip_all, fields(bdf = %self.bdf))]
    pub fn windows_hardware_id(&self) -> String {
        format!(
            "PCI\\VEN_{:04X}&DEV_{:04X}&SUBSYS_{:04X}{:04X}&REV_{:02X}",
            self.vendor_id,
            self.device_id,
            self.subsystem_id,
            self.subsystem_vendor_id,
            self.revision
        )
    }

    /// The Windows class-code compatible ID, e.g. `PCI\CC_010000`.
    #[tracing::instrument(level = "trace", target = "ternvale::pci", skip_all, fields(bdf = %self.bdf))]
    pub fn windows_compatible_id(&self) -> String {
        format!("PCI\\CC_{:06X}", self.class_code & 0x00ff_ffff)
    }

    /// The line debug EDK2 prints when `PciBusDxe` finds this function.
    #[tracing::instrument(level = "trace", target = "ternvale::pci", skip_all, fields(bdf = %self.bdf))]
    pub fn edk2_discovered(&self) -> String {
        format!(
            "PciBus: Discovered PCI @ [{:02X}|{:02X}|{:02X}]  [VID = 0x{:04X}, DID = 0x{:04X}]",
            self.bdf.bus, self.bdf.device, self.bdf.function, self.vendor_id, self.device_id
        )
    }

    /// Log this function at INFO: the IDs a guest (and its driver INF
    /// matching) sees, and the EDK2 line that should match it.
    #[tracing::instrument(level = "debug", target = "ternvale::pci", skip_all, fields(bdf = %self.bdf, function))]
    pub fn log(&self, function: &str) {
        tracing::info!(
            target: "ternvale::pci",
            bdf = %self.bdf,
            function,
            pci_id = %self,
            subsystem = format!("{:04x}:{:04x}", self.subsystem_vendor_id, self.subsystem_id),
            class = format!("{:#08x}", self.class_code),
            revision = self.revision,
            windows_hwid = %self.windows_hardware_id(),
            windows_compat = %self.windows_compatible_id(),
            edk2 = %self.edk2_discovered(),
            "guest-visible pci function"
        );
    }
}

/// `vendor:device` in lower-case hex, as `lspci -n` prints it.
impl fmt::Display for PciIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:04x}:{:04x}", self.vendor_id, self.device_id)
    }
}
