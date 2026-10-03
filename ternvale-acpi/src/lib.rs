//! ACPI tables for ARM64 guests (ACPI 6.5).
//!
//! [`AcpiTables::build`] lays out an RSDP, XSDT, hardware-reduced FADT, and a
//! DSDT holding an empty `\_SB` scope at a base guest physical address, with
//! every pointer resolved. [`AcpiTables::write`] hands each table to a
//! guest-memory callback and logs its signature, length, and checksum at INFO
//! on `ternvale::acpi`. [`walk`] reads a table set back out of guest memory
//! (or a RAM image from another VMM) and [`compare`] diffs two sets by
//! signature. The crate does not depend on the VMM: callers pass the region
//! and the reader or writer.

pub mod aml;
mod compare;
mod dsdt;
mod dump;
mod error;
mod fadt;
mod rsdp;
mod sdt;
mod tables;
mod xsdt;

pub use compare::{compare, render_markdown, signature_diff, Row};
pub use dsdt::{dsdt, DSDT_REVISION, DSDT_SIGNATURE};
pub use dump::{find_rsdp, walk, walk_image, DumpedTable, MAX_TABLE_LEN, MAX_XSDT_ENTRIES};
pub use error::AcpiError;
pub use fadt::{
    fadt, PsciConduit, ARM_BOOT_ARCH_OFFSET, ARM_PSCI_COMPLIANT, ARM_PSCI_USE_HVC, DSDT_OFFSET,
    FADT_LEN, FADT_MINOR_REVISION, FADT_REVISION, FADT_SIGNATURE, FLAGS_OFFSET, HW_REDUCED_ACPI,
    HYPERVISOR_VENDOR_OFFSET, MINOR_VERSION_OFFSET, X_DSDT_OFFSET,
};
pub use rsdp::{
    rsdp, rsdp_checksums_ok, RSDP_LEN, RSDP_REVISION, RSDP_SIGNATURE, RSDP_XSDT_OFFSET,
};
pub use sdt::{byte_sum, checksum, table, SdtHeader, SDT_CHECKSUM_OFFSET, SDT_HEADER_LEN};
pub use tables::{AcpiTables, Table, BASE_ALIGN, TABLE_ALIGN};
pub use xsdt::{xsdt, xsdt_len, XSDT_REVISION, XSDT_SIGNATURE};

#[cfg(test)]
#[path = "checksum_tests.rs"]
mod checksum_tests;

#[cfg(test)]
#[path = "iasl_tests.rs"]
mod iasl_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-acpi");
    }
}
