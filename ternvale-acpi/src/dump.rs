//! Reads ACPI tables back out of guest memory by following pointers from the
//! RSDP: RSDP → XSDT (ACPI 6.5 §5.2.8) → each listed table, plus the DSDT the
//! FADT points at (§5.2.9 `X_DSDT`, else `DSDT`). Works on Ternvale's guest
//! memory and on a RAM image saved from another VMM (QEMU `pmemsave`).
//!
//! Every address and length here comes from guest memory, so each is bounded
//! before use: tables are at most [`MAX_TABLE_LEN`] bytes, the XSDT lists at
//! most [`MAX_XSDT_ENTRIES`] tables, and the reader must return exactly the
//! bytes asked for. A bad table checksum is reported, not fatal.

use std::fmt::Display;

use crate::fadt::{DSDT_OFFSET, X_DSDT_OFFSET};
use crate::rsdp::{rsdp_checksums_ok, RSDP_LEN, RSDP_SIGNATURE, RSDP_XSDT_OFFSET};
use crate::sdt::{byte_sum, SdtHeader, SDT_HEADER_LEN};
use crate::AcpiError;

/// Largest table the walker reads.
pub const MAX_TABLE_LEN: u32 = 1 << 20;
/// Most XSDT entries the walker follows.
pub const MAX_XSDT_ENTRIES: usize = 64;
/// Byte offset of the RSDP revision.
const RSDP_REVISION_OFFSET: usize = 15;

/// One table read back from memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpedTable {
    /// `RSD PTR ` for the RSDP, else the 4-byte SDT signature.
    pub signature: String,
    /// Guest physical address of the first byte.
    pub gpa: u64,
    /// The whole table.
    pub bytes: Vec<u8>,
}

impl DumpedTable {
    /// True for the RSDP, which has no SDT header.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(signature = %self.signature))]
    pub fn is_rsdp(&self) -> bool {
        self.signature.as_bytes() == RSDP_SIGNATURE
    }

    /// The header revision (RSDP byte 15, else SDT byte 8).
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(signature = %self.signature))]
    pub fn revision(&self) -> u8 {
        let offset = if self.is_rsdp() {
            RSDP_REVISION_OFFSET
        } else {
            8
        };
        self.bytes.get(offset).copied().unwrap_or(0)
    }

    /// True when the table sums to zero (both checksums for the RSDP).
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(signature = %self.signature))]
    pub fn checksum_ok(&self) -> bool {
        if self.is_rsdp() {
            return <&[u8; RSDP_LEN]>::try_from(self.bytes.as_slice()).is_ok_and(rsdp_checksums_ok);
        }
        byte_sum(&self.bytes) == 0
    }

    /// File name for a dump: `RSDP.dat`, `FACP.dat`, ...
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(signature = %self.signature))]
    pub fn file_name(&self) -> String {
        if self.is_rsdp() {
            return "RSDP.dat".to_string();
        }
        let safe: String = self
            .signature
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        format!("{safe}.dat")
    }
}

/// Every table reachable from the RSDP at `rsdp_gpa`, in walk order (RSDP,
/// XSDT, each XSDT entry, the DSDT right after the FADT). `read(gpa, len)`
/// returns guest bytes.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(rsdp = %format!("{rsdp_gpa:#x}"))
)]
pub fn walk<E: Display>(
    rsdp_gpa: u64,
    mut read: impl FnMut(u64, usize) -> Result<Vec<u8>, E>,
) -> Result<Vec<DumpedTable>, AcpiError> {
    let mut read_exact = |what: &str, gpa: u64, len: usize| -> Result<Vec<u8>, AcpiError> {
        let bytes = read(gpa, len).map_err(|source| AcpiError::Read {
            what: what.to_string(),
            gpa,
            reason: source.to_string(),
        })?;
        if bytes.len() != len {
            return Err(bad(
                what,
                gpa,
                format!("reader returned {} of {len} bytes", bytes.len()),
            ));
        }
        Ok(bytes)
    };
    let rsdp = read_exact("RSDP", rsdp_gpa, RSDP_LEN)?;
    if rsdp[..8] != RSDP_SIGNATURE {
        return Err(bad("RSDP", rsdp_gpa, "no \"RSD PTR \" signature".into()));
    }
    if !<&[u8; RSDP_LEN]>::try_from(rsdp.as_slice()).is_ok_and(rsdp_checksums_ok) {
        return Err(bad("RSDP", rsdp_gpa, "checksum mismatch".into()));
    }
    if rsdp[RSDP_REVISION_OFFSET] < 2 {
        return Err(bad("RSDP", rsdp_gpa, "revision < 2 has no XSDT".into()));
    }
    let xsdt_gpa = u64_at(&rsdp, RSDP_XSDT_OFFSET);
    if xsdt_gpa == 0 {
        return Err(bad("RSDP", rsdp_gpa, "XsdtAddress is zero".into()));
    }
    let mut tables = vec![DumpedTable {
        signature: String::from_utf8_lossy(&RSDP_SIGNATURE).into_owned(),
        gpa: rsdp_gpa,
        bytes: rsdp,
    }];
    let xsdt = read_sdt(&mut read_exact, xsdt_gpa)?;
    if xsdt.signature != "XSDT" {
        return Err(bad(
            "XSDT",
            xsdt_gpa,
            format!("signature is {:?}", xsdt.signature),
        ));
    }
    let entries: Vec<u64> = xsdt.bytes[SDT_HEADER_LEN..]
        .chunks_exact(8)
        .map(|chunk| u64_at(chunk, 0))
        .collect();
    if entries.len() > MAX_XSDT_ENTRIES {
        return Err(bad("XSDT", xsdt_gpa, format!("{} entries", entries.len())));
    }
    tables.push(xsdt);
    for gpa in entries {
        if gpa == 0 {
            tracing::warn!(target: "ternvale::acpi", "xsdt entry is zero; skipped");
            continue;
        }
        let table = read_sdt(&mut read_exact, gpa)?;
        let dsdt = (table.signature == "FACP")
            .then(|| fadt_dsdt(&table))
            .flatten();
        tables.push(table);
        if let Some(dsdt) = dsdt.filter(|d| tables.iter().all(|t| t.gpa != *d)) {
            tables.push(read_sdt(&mut read_exact, dsdt)?);
        }
    }
    for table in &tables {
        let ok = table.checksum_ok();
        tracing::debug!(
            target: "ternvale::acpi",
            signature = %table.signature,
            gpa = %format!("{:#x}", table.gpa),
            length = table.bytes.len(),
            checksum_ok = ok,
            "acpi table read back"
        );
        if !ok {
            tracing::warn!(target: "ternvale::acpi", signature = %table.signature, "acpi table checksum mismatch");
        }
    }
    Ok(tables)
}

/// Guest physical addresses of every `RSD PTR ` in `image` (which starts at
/// `base`) whose two checksums hold. EDK2 does not 16-byte align it, so
/// every offset is checked.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(base = %format!("{base:#x}"), len = image.len())
)]
pub fn find_rsdp(image: &[u8], base: u64) -> Vec<u64> {
    let mut found = Vec::new();
    let mut offset = 0;
    while let Some(hit) = image
        .get(offset..)
        .and_then(|rest| rest.windows(8).position(|w| w == RSDP_SIGNATURE))
    {
        let at = offset + hit;
        let valid = image
            .get(at..at + RSDP_LEN)
            .and_then(|bytes| <&[u8; RSDP_LEN]>::try_from(bytes).ok())
            .is_some_and(rsdp_checksums_ok);
        if valid {
            found.push(base + at as u64);
        }
        offset = at + 1;
    }
    tracing::debug!(target: "ternvale::acpi", candidates = found.len(), "rsdp scan");
    found
}

/// Walk the tables in a RAM image starting at `base`, trying each RSDP
/// candidate until one walks cleanly.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(base = %format!("{base:#x}"), len = image.len())
)]
pub fn walk_image(image: &[u8], base: u64) -> Result<Vec<DumpedTable>, AcpiError> {
    let read = |gpa: u64, len: usize| -> Result<Vec<u8>, String> {
        let start = gpa
            .checked_sub(base)
            .and_then(|off| usize::try_from(off).ok())
            .ok_or_else(|| "below the image".to_string())?;
        start
            .checked_add(len)
            .and_then(|end| image.get(start..end))
            .map(<[u8]>::to_vec)
            .ok_or_else(|| "past the end of the image".to_string())
    };
    for rsdp in find_rsdp(image, base) {
        match walk(rsdp, read) {
            Ok(tables) => return Ok(tables),
            Err(error) => {
                tracing::warn!(target: "ternvale::acpi", rsdp = %format!("{rsdp:#x}"), error = %error, "rsdp candidate rejected");
            }
        }
    }
    tracing::warn!(target: "ternvale::acpi", "no rsdp in the image walks cleanly");
    Err(AcpiError::NoRsdp {
        base,
        len: image.len() as u64,
    })
}

fn read_sdt(
    read_exact: &mut impl FnMut(&str, u64, usize) -> Result<Vec<u8>, AcpiError>,
    gpa: u64,
) -> Result<DumpedTable, AcpiError> {
    let header = SdtHeader::parse(&read_exact("table header", gpa, SDT_HEADER_LEN)?)?;
    let signature = String::from_utf8_lossy(&header.signature).into_owned();
    if !(SDT_HEADER_LEN as u32..=MAX_TABLE_LEN).contains(&header.length) {
        return Err(bad(&signature, gpa, format!("length {}", header.length)));
    }
    let bytes = read_exact(&signature, gpa, header.length as usize)?;
    Ok(DumpedTable {
        signature,
        gpa,
        bytes,
    })
}

/// `X_DSDT` if the FADT is long enough and it is non-zero, else `DSDT`.
fn fadt_dsdt(fadt: &DumpedTable) -> Option<u64> {
    let x_dsdt = fadt
        .bytes
        .get(X_DSDT_OFFSET..X_DSDT_OFFSET + 8)
        .map(|b| u64_at(b, 0))
        .filter(|gpa| *gpa != 0);
    x_dsdt.or_else(|| {
        fadt.bytes
            .get(DSDT_OFFSET..DSDT_OFFSET + 4)
            .map(|b| u64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])))
            .filter(|gpa| *gpa != 0)
    })
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(raw)
}

fn bad(what: &str, gpa: u64, reason: String) -> AcpiError {
    tracing::warn!(target: "ternvale::acpi", what, gpa = %format!("{gpa:#x}"), reason = %reason, "bad acpi table in memory");
    AcpiError::BadTable {
        what: what.to_string(),
        gpa,
        reason,
    }
}

#[cfg(test)]
#[path = "dump_tests.rs"]
mod tests;
