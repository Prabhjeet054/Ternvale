//! Checksum fault injection for `acpi-dump`, used to prove that
//! `scripts/acpi-check.sh` catches a corrupt table
//! (`ACPI_CHECK_CORRUPT=GTDT ./scripts/acpi-check.sh`).
//!
//! `TERNVALE_ACPI_CORRUPT=<SIG>` (`RSDP` for the RSDP) adds 1 to that table's
//! checksum byte after the tables are built and walked, just before they are
//! written, so its bytes sum to 1 mod 256. The checksum byte is SDT offset 9
//! (ACPI 6.5 §5.2.6, Table 5.4) or RSDP offset 8 (§5.2.5.3, Table 5.3).
//! It only acts in builds with the `acpi-fault-injection` feature; other
//! builds log a WARN and leave the tables alone. The VMM's own drift check
//! (`ternvale_acpi::verify`) rejects such tables before boot, which is why the
//! fault goes in after it.

use anyhow::{bail, Context, Result};
use ternvale_acpi::{DumpedTable, RSDP_CHECKSUM_OFFSET, SDT_CHECKSUM_OFFSET};

/// Environment variable naming the table to corrupt.
pub const CORRUPT_ENV: &str = "TERNVALE_ACPI_CORRUPT";

/// Where a checksum byte was changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Corruption {
    /// The table's signature (`RSD PTR ` for the RSDP).
    pub signature: String,
    /// Guest physical address of the table.
    pub gpa: u64,
    /// Offset of the checksum byte in the table.
    pub offset: usize,
    /// The correct checksum byte.
    pub was: u8,
    /// The byte written instead.
    pub now: u8,
}

/// Add 1 to the checksum byte of the first table named `signature`
/// (`RSDP` or `RSD PTR ` for the RSDP).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(signature))]
pub fn corrupt_checksum(tables: &mut [DumpedTable], signature: &str) -> Result<Corruption> {
    let wanted = if signature == "RSDP" {
        "RSD PTR "
    } else {
        signature
    };
    let Some(table) = tables.iter_mut().find(|t| t.signature == wanted) else {
        let present: Vec<&str> = tables.iter().map(|t| t.signature.trim_end()).collect();
        bail!(
            "no {signature} table to corrupt (have {})",
            present.join(", ")
        );
    };
    let offset = if table.is_rsdp() {
        RSDP_CHECKSUM_OFFSET
    } else {
        SDT_CHECKSUM_OFFSET
    };
    let byte = table.bytes.get_mut(offset).with_context(|| {
        format!("{signature} is too short to have a checksum at offset {offset}")
    })?;
    let was = *byte;
    *byte = was.wrapping_add(1);
    let corruption = Corruption {
        signature: table.signature.clone(),
        gpa: table.gpa,
        offset,
        was,
        now: *byte,
    };
    tracing::warn!(
        target: "ternvale::cli",
        signature = %corruption.signature.trim_end(),
        gpa = %format!("{:#x}", corruption.gpa),
        offset,
        was = %format!("{was:#04x}"),
        now = %format!("{:#04x}", corruption.now),
        "acpi fault injection: checksum byte corrupted"
    );
    Ok(corruption)
}

/// Corrupt the table named by `spec` (the value of [`CORRUPT_ENV`]) when this
/// build has the `acpi-fault-injection` feature. Returns what was changed.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(spec = ?spec))]
pub fn apply(tables: &mut [DumpedTable], spec: Option<&str>) -> Result<Option<Corruption>> {
    let Some(signature) = spec.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if !cfg!(feature = "acpi-fault-injection") {
        tracing::warn!(
            target: "ternvale::cli",
            env = CORRUPT_ENV,
            signature,
            "ignored: this build has no acpi-fault-injection feature"
        );
        return Ok(None);
    }
    corrupt_checksum(tables, signature)
        .map(Some)
        .with_context(|| format!("{CORRUPT_ENV}={signature}"))
}

/// [`apply`] with the value of [`CORRUPT_ENV`].
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn apply_from_env(tables: &mut [DumpedTable]) -> Result<Option<Corruption>> {
    let spec = std::env::var(CORRUPT_ENV).ok();
    apply(tables, spec.as_deref())
}

#[cfg(test)]
#[path = "acpi_fault_tests.rs"]
mod tests;
