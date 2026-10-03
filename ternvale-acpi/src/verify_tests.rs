use super::*;
use crate::config::tests::sample;
use crate::sdt::{checksum, SDT_CHECKSUM_OFFSET, SDT_HEADER_LEN};
use crate::{walk_image, AcpiTables};

const BASE: u64 = 0x0910_0000;

fn dumped(config: &AcpiConfig) -> Vec<DumpedTable> {
    let tables = AcpiTables::build(BASE, 0x2_0000, config).expect("build");
    let mut image = vec![0u8; 0x2_0000];
    for table in tables.tables() {
        let at = (table.gpa - BASE) as usize;
        image[at..at + table.bytes.len()].copy_from_slice(&table.bytes);
    }
    walk_image(&image, BASE).expect("walk")
}

/// Change bytes `at` of table `signature` and fix its checksum, as a builder
/// bug (not memory corruption) would.
fn tamper(tables: &mut [DumpedTable], signature: &str, at: usize, value: &[u8]) {
    let table = tables
        .iter_mut()
        .find(|t| t.signature == signature)
        .expect("table");
    table.bytes[at..at + value.len()].copy_from_slice(value);
    table.bytes[SDT_CHECKSUM_OFFSET] = 0;
    table.bytes[SDT_CHECKSUM_OFFSET] = checksum(&table.bytes);
}

fn problems(tables: &[DumpedTable], config: &AcpiConfig) -> Vec<String> {
    match verify(tables, config) {
        Err(AcpiError::Drift { problems }) => problems,
        other => panic!("expected drift, got {other:?}"),
    }
}

#[test]
fn built_tables_verify_against_their_config() {
    let config = sample(4);
    let tables = dumped(&config);
    let names: Vec<_> = tables.iter().map(|t| t.signature.as_str()).collect();
    assert_eq!(names.len(), 9, "{names:?}");
    verify(&tables, &config).expect("no drift");
}

#[test]
fn every_drifted_field_is_named() {
    let config = sample(2);
    let mut tables = dumped(&config);
    let gicd = SDT_HEADER_LEN + 8 + 2 * 82 + 8;
    tamper(&mut tables, "APIC", gicd, &0x0801_0000u64.to_le_bytes());
    tamper(&mut tables, "GTDT", 56, &31u32.to_le_bytes());
    tamper(&mut tables, "MCFG", 55, &[7]);
    tamper(&mut tables, "SPCR", 44, &0x0900_1000u64.to_le_bytes());
    tamper(&mut tables, "DBG2", 44 + 34, &0x2000u32.to_le_bytes());
    let found = problems(&tables, &config);
    let text = found.join("\n");
    for needle in [
        "APIC GICD (base, version): expected [(8000000, 3)], found [(8010000, 3)]",
        "GTDT timer GSIVs: expected [1d, 1e, 1b, 1a], found [1d, 1f, 1b, 1a]",
        "MCFG allocations",
        "SPCR base",
        "DBG2 devices",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in\n{text}");
    }
    assert_eq!(found.len(), 5, "{text}");
}

#[test]
fn config_changes_are_drift_too() {
    let config = sample(2);
    let tables = dumped(&config);
    let mut moved = config.clone();
    moved.mpidrs[1] = 0x100;
    moved.gic.redist_len = 0x4_0000;
    moved.timer.always_on = false;
    let text = problems(&tables, &moved).join("\n");
    assert!(text.contains("APIC GICC 1 MPIDR"), "{text}");
    assert!(text.contains("APIC GICR"), "{text}");
    assert!(text.contains("GTDT always-on"), "{text}");
    let mut more = config;
    more.mpidrs.push(2);
    assert!(problems(&tables, &more)
        .join("\n")
        .contains("APIC GICC count: expected 3, found 2"));
}

#[test]
fn missing_tables_and_bad_checksums_are_drift() {
    let config = sample(1);
    let mut tables = dumped(&config);
    tables.retain(|t| t.signature != "GTDT");
    let spcr = tables
        .iter_mut()
        .find(|t| t.signature == "SPCR")
        .expect("spcr");
    spcr.bytes[SDT_CHECKSUM_OFFSET] ^= 1;
    let text = problems(&tables, &config).join("\n");
    assert!(text.contains("table set"), "{text}");
    assert!(text.contains("SPCR: checksum"), "{text}");
}

#[test]
fn undecodable_tables_are_drift_not_panics() {
    let config = sample(1);
    let mut tables = dumped(&config);
    let madt = tables
        .iter_mut()
        .find(|t| t.signature == "APIC")
        .expect("apic");
    madt.bytes.truncate(40);
    let text = problems(&tables, &config).join("\n");
    assert!(text.contains("APIC:"), "{text}");
}
