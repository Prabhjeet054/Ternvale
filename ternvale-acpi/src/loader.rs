//! QEMU's ACPI linker/loader: the fw_cfg files EDK2 ArmVirtQemu installs
//! ACPI tables from.
//!
//! EDK2 (edk2-stable202308, the pinned firmware) has no fixed RSDP address
//! and takes no RSDP pointer from the VMM. `PlatformHasAcpiDtDxe` picks ACPI
//! over the device tree only when fw_cfg has `etc/table-loader`. Then
//! `QemuFwCfgAcpi.c` (`InstallQemuFwCfgTables`) runs the loader script. It
//! downloads each `ALLOCATE`d file into ACPI NVS memory, adds the pointee
//! file's base to every `ADD_POINTER` field, and stores
//! `CalculateCheckSum8(start..start+len)` at every `ADD_CHECKSUM` result
//! offset. In a second pass, each `ADD_POINTER` target with a valid header
//! and zero byte sum goes to `EFI_ACPI_TABLE_PROTOCOL.InstallAcpiTable`,
//! except the RSDT and XSDT. `AcpiTableDxe` then builds its own RSDP and XSDT
//! and publishes them as the ACPI 2.0 UEFI configuration table. Layouts follow
//! `OvmfPkg/Include/IndustryStandard/QemuLoader.h` and QEMU
//! `hw/acpi/bios-linker-loader.c`.
//!
//! Like QEMU, [`LoaderBlobs`] uses two files: the RSDP alone, and every other
//! table at its original offset from the XSDT. Each pointer field holds an
//! offset into `etc/acpi/tables`. Each checksum byte is zeroed, because
//! `CalculateCheckSum8` sums the result byte too.
//! TODO(verify): confirmed by reading the EDK2 sources and by the
//! `firmware-acpi` boot scenario (UEFI shell `dmem`) against the pinned build
//! only. Newer EDK2 releases may move this code.

use crate::dump::DumpedTable;
use crate::fadt::{DSDT_OFFSET, X_DSDT_OFFSET};
use crate::loader_command::{bad_loader, LoaderCommand, Zone, LOADER_ENTRY_LEN};
use crate::rsdp::{
    RSDP_CHECKSUM_OFFSET, RSDP_EXT_CHECKSUM_OFFSET, RSDP_LEN, RSDP_V1_LEN, RSDP_XSDT_OFFSET,
};
use crate::sdt::{SDT_CHECKSUM_OFFSET, SDT_HEADER_LEN};
use crate::tables::Table;
use crate::AcpiError;

/// The loader script file. Its presence makes EDK2 choose ACPI.
pub const LOADER_FILE: &str = "etc/table-loader";
/// Every table except the RSDP.
pub const TABLES_FILE: &str = "etc/acpi/tables";
/// The RSDP.
pub const RSDP_FILE: &str = "etc/acpi/rsdp";
/// RSDP `RsdtAddress` (4 bytes), ACPI 6.5 §5.2.5.3.
const RSDP_RSDT_OFFSET: usize = 16;
/// FADT `FIRMWARE_CTRL` (4 bytes) and `X_FIRMWARE_CTRL` (8 bytes), §5.2.9.
const FIRMWARE_CTRL_OFFSET: usize = 36;
const X_FIRMWARE_CTRL_OFFSET: usize = 132;
/// QEMU's alignments: 16 for the RSDP (`FSEG`), 64 for the tables.
const RSDP_ALIGN: u32 = 16;
const TABLES_ALIGN: u32 = 64;
/// `etc/acpi/tables` limit, well above anything Ternvale builds.
const MAX_TABLES_BYTES: u64 = 1 << 20;

/// The three fw_cfg files for one table set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderBlobs {
    /// `etc/acpi/rsdp`: the RSDP, `XsdtAddress` relative to `etc/acpi/tables`.
    pub rsdp: Vec<u8>,
    /// `etc/acpi/tables`: XSDT first, every pointer relative to this file.
    pub tables: Vec<u8>,
    /// The `etc/table-loader` script, in execution order.
    pub commands: Vec<LoaderCommand>,
}

/// A table the loader blobs are built from: built or read back from memory.
#[derive(Debug, Clone, Copy)]
pub struct TableRef<'a> {
    /// `RSD PTR ` or the 4-byte SDT signature.
    pub signature: &'a str,
    /// Guest physical address the pointers were resolved against.
    pub gpa: u64,
    /// The whole table.
    pub bytes: &'a [u8],
}

impl<'a> From<&'a Table> for TableRef<'a> {
    fn from(table: &'a Table) -> Self {
        Self {
            signature: table.signature,
            gpa: table.gpa,
            bytes: &table.bytes,
        }
    }
}

impl<'a> From<&'a DumpedTable> for TableRef<'a> {
    fn from(table: &'a DumpedTable) -> Self {
        Self {
            signature: &table.signature,
            gpa: table.gpa,
            bytes: &table.bytes,
        }
    }
}

impl LoaderBlobs {
    /// Relocatable copies of `tables` (one RSDP plus SDTs, pointers resolved
    /// against their `gpa`s) and the script that relocates them. Every
    /// pointer must name the first byte of a table in the set.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(tables = tables.len()))]
    pub fn build(tables: &[TableRef<'_>]) -> Result<Self, AcpiError> {
        let mut rsdps = tables.iter().filter(|t| t.signature == "RSD PTR ");
        let (Some(rsdp), None) = (rsdps.next(), rsdps.next()) else {
            return Err(bad_loader("the table set needs exactly one RSDP".into()));
        };
        if rsdp.bytes.len() != RSDP_LEN {
            return Err(bad_loader(format!("RSDP is {} bytes", rsdp.bytes.len())));
        }
        let mut sdts: Vec<&TableRef<'_>> = tables
            .iter()
            .filter(|t| t.signature != "RSD PTR ")
            .collect();
        sdts.sort_by_key(|t| t.gpa);
        let Some(first) = sdts.first() else {
            return Err(bad_loader("no tables besides the RSDP".into()));
        };
        let start = first.gpa;
        let mut blob = Vec::new();
        let mut offsets = Vec::with_capacity(sdts.len());
        for sdt in &sdts {
            if sdt.bytes.len() < SDT_HEADER_LEN {
                return Err(bad_loader(format!(
                    "{} is {} bytes",
                    sdt.signature,
                    sdt.bytes.len()
                )));
            }
            let offset = sdt.gpa - start;
            let end = offset + sdt.bytes.len() as u64;
            if offset < blob.len() as u64 || end > MAX_TABLES_BYTES {
                return Err(bad_loader(format!(
                    "{} at {:#x} overlaps or is too far",
                    sdt.signature, sdt.gpa
                )));
            }
            blob.resize(offset as usize, 0);
            blob.extend_from_slice(sdt.bytes);
            offsets.push((sdt.signature, offset as usize, sdt.bytes.len()));
        }
        let target = |value: u64, field: &str| -> Result<u64, AcpiError> {
            if sdts.iter().any(|t| t.gpa == value) {
                Ok(value - start)
            } else {
                Err(bad_loader(format!(
                    "{field} {value:#x} is not the start of a table in the set"
                )))
            }
        };
        let mut commands = vec![
            allocate(RSDP_FILE, RSDP_ALIGN, Zone::FSeg),
            allocate(TABLES_FILE, TABLES_ALIGN, Zone::High),
        ];
        for &(signature, at, len) in &offsets {
            for (field, size) in pointer_fields(signature, len) {
                let pos = at + field;
                let value = read_le(&blob[pos..pos + size]);
                if value == 0 {
                    continue;
                }
                let relative = target(value, &format!("{signature}+{field:#x}"))?;
                blob[pos..pos + size].copy_from_slice(&relative.to_le_bytes()[..size]);
                commands.push(add_pointer(TABLES_FILE, pos, size));
            }
        }
        for &(_, at, len) in &offsets {
            blob[at + SDT_CHECKSUM_OFFSET] = 0;
            commands.push(add_checksum(TABLES_FILE, at + SDT_CHECKSUM_OFFSET, at, len));
        }
        let mut rsdp_blob = rsdp.bytes.to_vec();
        for (field, size) in [(RSDP_RSDT_OFFSET, 4), (RSDP_XSDT_OFFSET, 8)] {
            let value = read_le(&rsdp_blob[field..field + size]);
            if value != 0 {
                let relative = target(value, &format!("RSDP+{field:#x}"))?;
                rsdp_blob[field..field + size].copy_from_slice(&relative.to_le_bytes()[..size]);
                commands.push(add_pointer(RSDP_FILE, field, size));
            }
        }
        rsdp_blob[RSDP_CHECKSUM_OFFSET] = 0;
        rsdp_blob[RSDP_EXT_CHECKSUM_OFFSET] = 0;
        commands.push(add_checksum(
            RSDP_FILE,
            RSDP_CHECKSUM_OFFSET,
            0,
            RSDP_V1_LEN,
        ));
        commands.push(add_checksum(
            RSDP_FILE,
            RSDP_EXT_CHECKSUM_OFFSET,
            0,
            RSDP_LEN,
        ));
        tracing::info!(
            target: "ternvale::acpi",
            tables = sdts.len(),
            tables_bytes = blob.len(),
            commands = commands.len(),
            "acpi loader script built for fw_cfg"
        );
        Ok(Self {
            rsdp: rsdp_blob,
            tables: blob,
            commands,
        })
    }

    /// The encoded `etc/table-loader` file.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(commands = self.commands.len()))]
    pub fn loader(&self) -> Result<Vec<u8>, AcpiError> {
        let mut out = Vec::with_capacity(self.commands.len() * LOADER_ENTRY_LEN);
        for command in &self.commands {
            out.extend_from_slice(&command.encode()?);
        }
        Ok(out)
    }

    /// `(name, contents)` for fw_cfg: RSDP, tables, then the loader script.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all)]
    pub fn files(&self) -> Result<Vec<(String, Vec<u8>)>, AcpiError> {
        Ok(vec![
            (RSDP_FILE.to_string(), self.rsdp.clone()),
            (TABLES_FILE.to_string(), self.tables.clone()),
            (LOADER_FILE.to_string(), self.loader()?),
        ])
    }
}

/// Table pointer fields `(offset, size)` an SDT can hold. Device addresses
/// (GAS, ECAM, GIC bases) are not table pointers and stay absolute.
fn pointer_fields(signature: &str, len: usize) -> Vec<(usize, usize)> {
    match signature {
        "XSDT" => (SDT_HEADER_LEN..len.saturating_sub(7))
            .step_by(8)
            .map(|at| (at, 8))
            .collect(),
        "FACP" => [
            (FIRMWARE_CTRL_OFFSET, 4),
            (DSDT_OFFSET, 4),
            (X_FIRMWARE_CTRL_OFFSET, 8),
            (X_DSDT_OFFSET, 8),
        ]
        .into_iter()
        .filter(|(at, size)| at + size <= len)
        .collect(),
        _ => Vec::new(),
    }
}

fn read_le(bytes: &[u8]) -> u64 {
    let mut value = [0u8; 8];
    value[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(value)
}

fn allocate(file: &str, align: u32, zone: Zone) -> LoaderCommand {
    LoaderCommand::Allocate {
        file: file.to_string(),
        align,
        zone,
    }
}

fn add_pointer(file: &str, offset: usize, size: usize) -> LoaderCommand {
    LoaderCommand::AddPointer {
        pointer: file.to_string(),
        pointee: TABLES_FILE.to_string(),
        offset: offset as u32,
        size: size as u8,
    }
}

fn add_checksum(file: &str, result: usize, start: usize, len: usize) -> LoaderCommand {
    LoaderCommand::AddChecksum {
        file: file.to_string(),
        result: result as u32,
        start: start as u32,
        len: len as u32,
    }
}

#[cfg(test)]
#[path = "loader_tests.rs"]
mod tests;
