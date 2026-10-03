//! ACPI tables for ARM64 guests (ACPI 6.5).
//!
//! [`AcpiTables::build`] lays out an RSDP, XSDT, hardware-reduced FADT, MADT
//! (GICv3), GTDT, MCFG, SPCR, DBG2, and a DSDT whose `\_SB` holds the PCIe
//! root bridge `PCI0` at a base guest physical address, with every pointer
//! resolved, from an
//! [`AcpiConfig`] the VMM fills from its platform map. [`AcpiTables::write`]
//! hands each table to a guest-memory callback and logs its signature, length,
//! and checksum at INFO on `ternvale::acpi`. [`walk`] reads a table set back
//! out of guest memory (or a RAM image from another VMM), [`verify`] checks it
//! against the config, and [`compare`] diffs two sets by signature.
//! [`LoaderBlobs`] turns a table set into QEMU's fw_cfg linker/loader files,
//! which is how EDK2 ArmVirtQemu receives ACPI tables. The crate
//! does not depend on the VMM: callers pass the region and the reader or
//! writer.

pub mod aml;
pub mod aml_data;
pub mod aml_resource;
mod compare;
mod config;
mod dbg2;
mod decode;
mod dsdt;
mod dump;
mod error;
mod fadt;
mod gas;
mod gtdt;
mod loader;
mod loader_command;
mod madt;
mod mcfg;
mod pci_root;
mod rsdp;
mod sdt;
mod spcr;
mod tables;
mod verify;
mod xsdt;

pub use compare::{compare, render_markdown, signature_diff, Row};
pub use config::{
    ppi_gsiv, spi_gsiv, AcpiConfig, EcamConfig, GicConfig, PciConfig, TimerConfig, UartConfig,
    MAX_CPUS,
};
pub use dbg2::{dbg2, DBG2_REVISION, DBG2_SIGNATURE, PORT_SERIAL, SUBTYPE_PL011};
pub use decode::{
    decode_dbg2, decode_gtdt, decode_madt, decode_mcfg, decode_spcr, Dbg2Device, GiccInfo,
    GtdtInfo, MadtInfo, McfgAllocation, SpcrInfo,
};
pub use dsdt::{dsdt, DSDT_REVISION, DSDT_SIGNATURE};
pub use dump::{find_rsdp, walk, walk_image, DumpedTable, MAX_TABLE_LEN, MAX_XSDT_ENTRIES};
pub use error::AcpiError;
pub use fadt::{
    fadt, PsciConduit, ARM_BOOT_ARCH_OFFSET, ARM_PSCI_COMPLIANT, ARM_PSCI_USE_HVC, DSDT_OFFSET,
    FADT_LEN, FADT_MINOR_REVISION, FADT_REVISION, FADT_SIGNATURE, FLAGS_OFFSET, HW_REDUCED_ACPI,
    HYPERVISOR_VENDOR_OFFSET, MINOR_VERSION_OFFSET, X_DSDT_OFFSET,
};
pub use gas::{Gas, ACCESS_DWORD, GAS_LEN, SPACE_SYSTEM_MEMORY};
pub use gtdt::{gtdt, GTDT_LEN, GTDT_REVISION, GTDT_SIGNATURE, TIMER_ALWAYS_ON};
pub use loader::{LoaderBlobs, TableRef, LOADER_FILE, RSDP_FILE, TABLES_FILE};
pub use loader_command::{LoaderCommand, Zone, FNAME_LEN, LOADER_ENTRY_LEN};
pub use madt::{
    madt, GICC_ENABLED, GICC_LEN, GICD_LEN, GICR_LEN, GIC_VERSION_3, MADT_REVISION, MADT_SIGNATURE,
};
pub use mcfg::{mcfg, MCFG_REVISION, MCFG_SIGNATURE};
pub use pci_root::{
    pci_root, prt_routes, PrtRoute, INTX_PINS, MOTHERBOARD_HID, PCIE_ROOT_HID, PCI_ROOT_CID,
    ROOT_BUS_DEVICES,
};
pub use rsdp::{
    rsdp, rsdp_checksums_ok, RSDP_CHECKSUM_OFFSET, RSDP_LEN, RSDP_REVISION, RSDP_SIGNATURE,
    RSDP_XSDT_OFFSET,
};
pub use sdt::{byte_sum, checksum, table, SdtHeader, SDT_CHECKSUM_OFFSET, SDT_HEADER_LEN};
pub use spcr::{
    spcr, BAUD_115200, INTERFACE_PL011, INTERRUPT_TYPE_GIC, SPCR_LEN, SPCR_REVISION, SPCR_SIGNATURE,
};
pub use tables::{AcpiTables, Table, BASE_ALIGN, SIGNATURES, TABLE_ALIGN};
pub use verify::verify;
pub use xsdt::{xsdt, xsdt_len, XSDT_REVISION, XSDT_SIGNATURE};

#[cfg(test)]
#[path = "checksum_tests.rs"]
mod checksum_tests;

#[cfg(test)]
#[path = "iasl_tests.rs"]
mod iasl_tests;

#[cfg(test)]
#[path = "iasl_dsdt_tests.rs"]
mod iasl_dsdt_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-acpi");
    }
}
