//! Compares two dumped table sets, for example Ternvale's against QEMU
//! `-M virt`'s, by signature.
//!
//! Tables present in both are checked for header revision and checksum and,
//! for the FADT, the fields an ARM OS keys on (ACPI 6.5 §5.2.9): minor
//! version, `HW_REDUCED_ACPI`, the ARM boot flags, and whether the 32-bit
//! `DSDT` field is used. Lengths are shown but not counted as differences:
//! the XSDT and DSDT grow with the table set and the device list.

use crate::dump::DumpedTable;
use crate::fadt::{ARM_BOOT_ARCH_OFFSET, DSDT_OFFSET, FLAGS_OFFSET, MINOR_VERSION_OFFSET};

/// One signature in either set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Table signature (`RSD PTR ` for the RSDP).
    pub signature: String,
    /// `(revision, length)` in the first set.
    pub ours: Option<(u8, usize)>,
    /// `(revision, length)` in the second set.
    pub theirs: Option<(u8, usize)>,
    /// Field differences when both sets have the table.
    pub differences: Vec<String>,
}

impl Row {
    /// `same`, `differs`, `only ours`, or `only theirs`.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(signature = %self.signature))]
    pub fn verdict(&self) -> &'static str {
        match (self.ours, self.theirs) {
            (Some(_), None) => "only ours",
            (None, Some(_)) => "only theirs",
            _ if self.differences.is_empty() => "same",
            _ => "differs",
        }
    }
}

/// One row per signature: `ours` in order, then signatures only in `theirs`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(ours = ours.len(), theirs = theirs.len())
)]
pub fn compare(ours: &[DumpedTable], theirs: &[DumpedTable]) -> Vec<Row> {
    let find = |set: &'_ [DumpedTable], sig: &str| set.iter().find(|t| t.signature == sig).cloned();
    let mut rows: Vec<Row> = ours
        .iter()
        .map(|table| row(Some(table.clone()), find(theirs, &table.signature)))
        .collect();
    rows.extend(
        theirs
            .iter()
            .filter(|t| find(ours, &t.signature).is_none())
            .map(|t| row(None, Some(t.clone()))),
    );
    for row in &rows {
        tracing::debug!(
            target: "ternvale::acpi",
            signature = %row.signature,
            verdict = row.verdict(),
            differences = ?row.differences,
            "acpi table compared"
        );
    }
    rows
}

/// Signatures in `ours` but not `theirs`, and in `theirs` but not `ours`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all)]
pub fn signature_diff(rows: &[Row]) -> (Vec<String>, Vec<String>) {
    let pick = |verdict: &str| {
        rows.iter()
            .filter(|row| row.verdict() == verdict)
            .map(|row| row.signature.trim_end().to_string())
            .collect()
    };
    (pick("only ours"), pick("only theirs"))
}

/// A markdown table of `rows`, with column headings `ours` and `theirs`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(rows = rows.len()))]
pub fn render_markdown(rows: &[Row], ours: &str, theirs: &str) -> String {
    let cell = |side: Option<(u8, usize)>| {
        side.map_or("-".to_string(), |(rev, len)| format!("rev {rev}, {len} B"))
    };
    let mut out = format!(
        "| Signature | {ours} | {theirs} | Verdict | Differences |\n|---|---|---|---|---|\n"
    );
    for row in rows {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            row.signature.trim_end(),
            cell(row.ours),
            cell(row.theirs),
            row.verdict(),
            if row.differences.is_empty() {
                "-".to_string()
            } else {
                row.differences.join("; ")
            }
        ));
    }
    out
}

fn row(ours: Option<DumpedTable>, theirs: Option<DumpedTable>) -> Row {
    let summary = |t: &DumpedTable| (t.revision(), t.bytes.len());
    let signature = ours
        .as_ref()
        .or(theirs.as_ref())
        .map_or_else(String::new, |t| t.signature.clone());
    let mut differences = Vec::new();
    if let (Some(a), Some(b)) = (&ours, &theirs) {
        let mut field = |name: &str, x: String, y: String| {
            if x != y {
                differences.push(format!("{name} {x} vs {y}"));
            }
        };
        field(
            "revision",
            a.revision().to_string(),
            b.revision().to_string(),
        );
        field(
            "checksum ok",
            a.checksum_ok().to_string(),
            b.checksum_ok().to_string(),
        );
        if signature == "FACP" {
            for ((label, x), (_, y)) in fadt_fields(a).into_iter().zip(fadt_fields(b)) {
                field(label, x, y);
            }
        }
    }
    Row {
        signature,
        ours: ours.as_ref().map(summary),
        theirs: theirs.as_ref().map(summary),
        differences,
    }
}

fn fadt_fields(fadt: &DumpedTable) -> [(&'static str, String); 4] {
    let bytes = &fadt.bytes;
    let le = |at: usize, len: usize| {
        bytes.get(at..at + len).map_or(0u64, |raw| {
            raw.iter().rev().fold(0, |v, b| (v << 8) | u64::from(*b))
        })
    };
    [
        ("minor version", le(MINOR_VERSION_OFFSET, 1).to_string()),
        ("flags", format!("{:#x}", le(FLAGS_OFFSET, 4))),
        (
            "arm boot flags",
            format!("{:#x}", le(ARM_BOOT_ARCH_OFFSET, 2)),
        ),
        ("32-bit DSDT set", (le(DSDT_OFFSET, 4) != 0).to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sdt(signature: &str, revision: u8, len: usize) -> DumpedTable {
        let mut bytes = vec![0u8; len];
        bytes[..4].copy_from_slice(signature.as_bytes());
        bytes[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        bytes[8] = revision;
        bytes[9] = crate::sdt::checksum(&bytes);
        DumpedTable {
            signature: signature.to_string(),
            gpa: 0,
            bytes,
        }
    }

    fn fadt(minor: u8, dsdt32: u32) -> DumpedTable {
        let mut table = sdt("FACP", 6, 276);
        table.bytes[MINOR_VERSION_OFFSET] = minor;
        table.bytes[FLAGS_OFFSET + 2] = 0x10;
        table.bytes[ARM_BOOT_ARCH_OFFSET] = 3;
        table.bytes[DSDT_OFFSET..DSDT_OFFSET + 4].copy_from_slice(&dsdt32.to_le_bytes());
        table.bytes[9] = 0;
        table.bytes[9] = crate::sdt::checksum(&table.bytes);
        table
    }

    #[test]
    fn rows_cover_both_sets_with_verdicts() {
        let ours = [sdt("XSDT", 1, 44), fadt(5, 0), sdt("DSDT", 2, 43)];
        let theirs = [
            sdt("XSDT", 1, 100),
            fadt(3, 0x4cb4_1018),
            sdt("DSDT", 2, 5320),
            sdt("APIC", 4, 172),
        ];
        let rows = compare(&ours, &theirs);
        let verdicts: Vec<_> = rows
            .iter()
            .map(|r| (r.signature.as_str(), r.verdict()))
            .collect();
        assert_eq!(
            verdicts,
            [
                ("XSDT", "same"),
                ("FACP", "differs"),
                ("DSDT", "same"),
                ("APIC", "only theirs")
            ]
        );
        assert_eq!(
            rows[1].differences,
            ["minor version 5 vs 3", "32-bit DSDT set false vs true"]
        );
        assert_eq!(rows[0].theirs, Some((1, 100)));
        assert_eq!(signature_diff(&rows), (vec![], vec!["APIC".to_string()]));
    }

    #[test]
    fn revision_and_checksum_differences_are_listed() {
        let mut broken = sdt("GTDT", 3, 64);
        broken.bytes[40] = 1;
        let rows = compare(&[sdt("GTDT", 2, 64), sdt("SPCR", 2, 80)], &[broken]);
        assert_eq!(
            rows[0].differences,
            ["revision 2 vs 3", "checksum ok true vs false"]
        );
        assert_eq!(rows[1].verdict(), "only ours");
        assert_eq!(signature_diff(&rows).0, ["SPCR"]);
    }

    #[test]
    fn markdown_has_one_line_per_row() {
        let rows = compare(
            &[sdt("DSDT", 2, 43)],
            &[sdt("DSDT", 2, 5320), sdt("IORT", 5, 84)],
        );
        let text = render_markdown(&rows, "Ternvale", "QEMU");
        assert!(
            text.starts_with("| Signature | Ternvale | QEMU |"),
            "{text}"
        );
        assert!(
            text.contains("| `DSDT` | rev 2, 43 B | rev 2, 5320 B | same | - |"),
            "{text}"
        );
        assert!(
            text.contains("| `IORT` | - | rev 5, 84 B | only theirs | - |"),
            "{text}"
        );
        assert_eq!(text.lines().count(), 4);
    }
}
