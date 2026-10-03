//! `\_SB.PCI0`: the PCI Express root bridge in the DSDT.
//!
//! The devices behind it are not in the namespace. An OS finds them by
//! standard enumeration of the ECAM window the MCFG gives (PCI Firmware
//! Specification 3.3 §4.1.2). This device only tells it where that hierarchy
//! is and what it may use:
//!
//! - `_HID` `PNP0A08` (PCI Express root bridge) and `_CID` `PNP0A03` (PCI
//!   root bridge), ACPI 6.5 §6.1.5 and §6.1.2; `_UID` 0 (§6.1.12). The
//!   PCI Firmware Specification gives these two IDs to a PCIe host bridge.
//! - `_SEG` 0 and `_BBN` = the first ECAM bus (§6.5.6, §6.5.5), which pair the
//!   device with the MCFG allocation for that segment and bus.
//! - `_CCA` 1 (§6.2.17): DMA is cache-coherent, as the DTB's `dma-coherent`
//!   says. ACPI on Arm requires it; without it Linux treats devices as unable
//!   to DMA.
//! - `_PRT` (§6.2.13): INTA..INTD of each of the 32 devices on the root bus to
//!   a GSIV, using the same swizzle as the DTB `interrupt-map`. Source is
//!   `Zero`, so Source Index is the GSIV. No link devices: on a GIC an SPI is
//!   level-triggered, active-high, and Linux assumes exactly that for a GSIV
//!   `_PRT` entry when the interrupt model is GIC (`acpi_pci_irq_enable`).
//!   TODO(verify): that Windows does the same (QEMU uses `PNP0C0F` links).
//! - `_CRS` (§6.2.2): `WordBusNumber` for the ECAM buses and one producer
//!   `DWordMemory` for the 32-bit MMIO window. There is no I/O window: the
//!   platform has no PCI I/O space, so none is offered.
//! - `RES0` (`PNP0C02`, motherboard resources) consumes the ECAM range.
//!   PCI Firmware 3.3 §4.1.2 asks for the MCFG region to be reserved that way
//!   and not claimed in the root bridge's `_CRS` (TODO(verify) the exact
//!   wording). QEMU's `virt` puts the same device inside `PCI0`.
//!   TODO(verify): Windows' acceptance of this layout.
//!
//! No `_OSC` or `_DSM` (§6.2.11): the platform keeps control of the PCIe
//! native features (hotplug, AER, PME), and none of them exist here. Linux
//! logs `_OSC: platform retains control of PCIe features (AE_NOT_FOUND)`.
//! TODO(verify): that Windows binds a `PNP0A08` root bridge without `_OSC`.

use crate::aml::{device, name};
use crate::aml_data::{eisa_id, integer, package};
use crate::aml_resource::{dword_memory, resource_template, word_bus_number, Usage};
use crate::config::{spi_gsiv, AcpiConfig};
use crate::AcpiError;

/// `_HID` of a PCI Express root bridge.
pub const PCIE_ROOT_HID: &str = "PNP0A08";
/// `_CID` of a PCI root bridge.
pub const PCI_ROOT_CID: &str = "PNP0A03";
/// `_HID` of the motherboard-resources device holding the ECAM reservation.
pub const MOTHERBOARD_HID: &str = "PNP0C02";
/// Devices on one bus (PCI Local Bus 3.0: 5 device bits).
pub const ROOT_BUS_DEVICES: u8 = 32;
/// INTA..INTD.
pub const INTX_PINS: u8 = 4;
/// ECAM bytes per bus (PCIe Base 6.0 §7.2.2).
const ECAM_BUS_BYTES: u64 = 1 << 20;

/// One `_PRT` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrtRoute {
    /// Device number on the root bus.
    pub device: u8,
    /// Pin, 0 = INTA .. 3 = INTD (the `_PRT` encoding, not the config
    /// space `Interrupt Pin` register's 1..=4).
    pub pin: u8,
    /// Global System Interrupt (GIC INTID).
    pub gsiv: u32,
}

/// Every `_PRT` entry, device-major: 32 devices x 4 pins.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all)]
pub fn prt_routes(config: &AcpiConfig) -> Vec<PrtRoute> {
    (0..ROOT_BUS_DEVICES)
        .flat_map(|device| {
            (0..INTX_PINS).map(move |pin| PrtRoute {
                device,
                pin,
                gsiv: spi_gsiv(config.pci.intx_spis[usize::from((device + pin) % INTX_PINS)]),
            })
        })
        .collect()
}

/// `Device (PCI0) { ... }` for `config`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(
        buses = %format!("{}..={}", config.ecam.start_bus, config.ecam.end_bus),
        mmio = %format!("{:#x}+{:#x}", config.pci.mmio_base, config.pci.mmio_len)
    )
)]
pub fn pci_root(config: &AcpiConfig) -> Result<Vec<u8>, AcpiError> {
    let ecam = config.ecam;
    let buses = u64::from(ecam.end_bus) - u64::from(ecam.start_bus) + 1;
    let ecam_len = buses * ECAM_BUS_BYTES;
    let routes = prt_routes(config);
    let prt = routes
        .iter()
        .map(|route| {
            package(&[
                integer((u64::from(route.device) << 16) | 0xffff),
                integer(u64::from(route.pin)),
                integer(0),
                integer(u64::from(route.gsiv)),
            ])
        })
        .collect::<Result<Vec<_>, _>>()?;
    let crs = resource_template(&[
        word_bus_number(u16::from(ecam.start_bus), u16::from(ecam.end_bus))?,
        dword_memory(Usage::Producer, config.pci.mmio_base, config.pci.mmio_len)?,
    ])?;
    let res0 = device(
        "RES0",
        &[
            name("_HID", &eisa_id(MOTHERBOARD_HID)?)?,
            name(
                "_CRS",
                &resource_template(&[dword_memory(Usage::Consumer, ecam.base, ecam_len)?])?,
            )?,
        ]
        .concat(),
    )?;
    let body = [
        name("_HID", &eisa_id(PCIE_ROOT_HID)?)?,
        name("_CID", &eisa_id(PCI_ROOT_CID)?)?,
        name("_SEG", &integer(0))?,
        name("_BBN", &integer(u64::from(ecam.start_bus)))?,
        name("_UID", &integer(0))?,
        name("_CCA", &integer(1))?,
        name("_PRT", &package(&prt)?)?,
        name("_CRS", &crs)?,
        res0,
    ]
    .concat();
    let aml = device("PCI0", &body)?;
    tracing::debug!(
        target: "ternvale::acpi",
        bytes = aml.len(),
        prt_entries = routes.len(),
        ecam = %format!("{:#x}+{ecam_len:#x}", ecam.base),
        intx_gsivs = ?config.pci.intx_spis.map(spi_gsiv),
        "dsdt pci root bridge built"
    );
    Ok(aml)
}

#[cfg(test)]
#[path = "pci_root_tests.rs"]
mod tests;
