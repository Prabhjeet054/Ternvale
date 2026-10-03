//! Lays out the ACPI tables in one guest region and writes them.
//!
//! Order from the region base, each table 8-byte aligned: RSDP, XSDT, FACP,
//! DSDT, APIC, GTDT, MCFG, SPCR, DBG2 (the order [`crate::walk`] reads them
//! back in). The XSDT lists FACP, APIC, GTDT, MCFG, SPCR, and DBG2; the
//! FADT's `X_DSDT` points at the DSDT. Tables without pointers are encoded
//! first, then every address is laid out, then the FADT, XSDT, and RSDP are
//! built once with their final pointers.

use std::fmt::Display;

use crate::config::AcpiConfig;
use crate::dbg2::dbg2;
use crate::dsdt::dsdt;
use crate::fadt::{fadt, FADT_LEN};
use crate::gtdt::gtdt;
use crate::madt::madt;
use crate::mcfg::mcfg;
use crate::rsdp::{rsdp, RSDP_CHECKSUM_OFFSET, RSDP_LEN};
use crate::sdt::{byte_sum, SDT_CHECKSUM_OFFSET};
use crate::spcr::spcr;
use crate::xsdt::{xsdt, xsdt_len};
use crate::AcpiError;

/// Signatures in address order.
pub const SIGNATURES: [&str; 9] = [
    "RSD PTR ", "XSDT", "FACP", "DSDT", "APIC", "GTDT", "MCFG", "SPCR", "DBG2",
];
/// Tables the XSDT lists.
const XSDT_ENTRIES: usize = 6;

/// Alignment of every table after the RSDP.
pub const TABLE_ALIGN: u64 = 8;
/// Required alignment of the region base (the RSDP's IA-PC rule; harmless here).
pub const BASE_ALIGN: u64 = 16;

/// One encoded table at its guest physical address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// One of [`SIGNATURES`].
    pub signature: &'static str,
    /// Guest physical address of the first byte.
    pub gpa: u64,
    /// Encoded bytes, checksums filled.
    pub bytes: Vec<u8>,
}

impl Table {
    /// The checksum byte (the ACPI 1.0 checksum for the RSDP).
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(signature = self.signature))]
    pub fn checksum(&self) -> u8 {
        let offset = if self.signature == "RSD PTR " {
            RSDP_CHECKSUM_OFFSET
        } else {
            SDT_CHECKSUM_OFFSET
        };
        self.bytes.get(offset).copied().unwrap_or(0)
    }
}

/// Every table for one guest, laid out from `base`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpiTables {
    base: u64,
    tables: Vec<Table>,
}

impl AcpiTables {
    /// Lay out every table describing `config` from `base`, failing if they
    /// do not fit in `size` bytes.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::acpi",
        skip_all,
        fields(
            base = %format!("{base:#x}"),
            size = %format!("{size:#x}"),
            conduit = ?config.conduit,
            cpus = config.mpidrs.len()
        )
    )]
    pub fn build(base: u64, size: u64, config: &AcpiConfig) -> Result<Self, AcpiError> {
        if base % BASE_ALIGN != 0 {
            tracing::warn!(target: "ternvale::acpi", base = %format!("{base:#x}"), "acpi region misaligned");
            return Err(AcpiError::Misaligned { base });
        }
        config.validate()?;
        let leaves: [(&'static str, Vec<u8>); 5] = [
            ("APIC", madt(config)?),
            ("GTDT", gtdt(&config.timer)?),
            ("MCFG", mcfg(&config.ecam)?),
            ("SPCR", spcr(&config.uart)?),
            ("DBG2", dbg2(&config.uart)?),
        ];
        let dsdt = dsdt(config)?;
        let overflow = || {
            tracing::warn!(target: "ternvale::acpi", base = %format!("{base:#x}"), "acpi layout overflows");
            AcpiError::AddressOverflow { base }
        };
        let after = |gpa: u64, len: usize| -> Result<u64, AcpiError> {
            let end = gpa.checked_add(len as u64).ok_or_else(overflow)?;
            end.checked_next_multiple_of(TABLE_ALIGN)
                .ok_or_else(overflow)
        };
        let xsdt_gpa = after(base, RSDP_LEN)?;
        let fadt_gpa = after(xsdt_gpa, xsdt_len(XSDT_ENTRIES))?;
        let dsdt_gpa = after(fadt_gpa, FADT_LEN)?;
        let mut next = dsdt_gpa;
        let mut len = dsdt.len();
        let mut leaf_gpas = Vec::with_capacity(leaves.len());
        for (_, bytes) in &leaves {
            next = after(next, len)?;
            leaf_gpas.push(next);
            len = bytes.len();
        }
        let end = next.checked_add(len as u64).ok_or_else(overflow)?;
        let need = end - base;
        if need > size {
            let error = AcpiError::RegionTooSmall {
                base,
                need,
                have: size,
            };
            tracing::warn!(target: "ternvale::acpi", error = %error, "acpi tables do not fit");
            return Err(error);
        }
        let mut listed = vec![fadt_gpa];
        listed.extend_from_slice(&leaf_gpas);
        let mut tables = vec![
            Table {
                signature: "RSD PTR ",
                gpa: base,
                bytes: rsdp(xsdt_gpa).to_vec(),
            },
            Table {
                signature: "XSDT",
                gpa: xsdt_gpa,
                bytes: xsdt(&listed)?,
            },
            Table {
                signature: "FACP",
                gpa: fadt_gpa,
                bytes: fadt(dsdt_gpa, config.conduit)?,
            },
            Table {
                signature: "DSDT",
                gpa: dsdt_gpa,
                bytes: dsdt,
            },
        ];
        for ((signature, bytes), gpa) in leaves.into_iter().zip(leaf_gpas) {
            tables.push(Table {
                signature,
                gpa,
                bytes,
            });
        }
        tracing::debug!(
            target: "ternvale::acpi",
            tables = tables.len(),
            bytes = need,
            "acpi tables laid out"
        );
        Ok(Self { base, tables })
    }

    /// Guest physical address of the RSDP (the region base).
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all)]
    pub fn rsdp_gpa(&self) -> u64 {
        self.base
    }

    /// The tables in address order.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all)]
    pub fn tables(&self) -> &[Table] {
        &self.tables
    }

    /// Bytes from the region base to the end of the last table.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all)]
    pub fn used_bytes(&self) -> u64 {
        self.tables
            .last()
            .map_or(0, |last| last.gpa + last.bytes.len() as u64 - self.base)
    }

    /// Hand each table to `write(gpa, bytes)` in address order and log its
    /// signature, address, length, and checksum at INFO once it is written.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::acpi",
        skip_all,
        fields(rsdp = %format!("{:#x}", self.base))
    )]
    pub fn write<E: Display>(
        &self,
        mut write: impl FnMut(u64, &[u8]) -> Result<(), E>,
    ) -> Result<(), AcpiError> {
        for table in &self.tables {
            if let Err(source) = write(table.gpa, &table.bytes) {
                let error = AcpiError::Write {
                    signature: table.signature.to_string(),
                    gpa: table.gpa,
                    reason: source.to_string(),
                };
                tracing::error!(target: "ternvale::acpi", error = %error, "acpi table write failed");
                return Err(error);
            }
            tracing::info!(
                target: "ternvale::acpi",
                signature = table.signature,
                gpa = %format!("{:#x}", table.gpa),
                length = table.bytes.len(),
                checksum = %format!("{:#04x}", table.checksum()),
                sum_ok = byte_sum(&table.bytes) == 0,
                "acpi table written"
            );
        }
        tracing::info!(
            target: "ternvale::acpi",
            rsdp = %format!("{:#x}", self.base),
            tables = self.tables.len(),
            bytes = self.used_bytes(),
            "acpi tables written"
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "tables_tests.rs"]
mod tests;
