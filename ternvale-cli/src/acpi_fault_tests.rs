use super::*;

fn tables() -> Vec<DumpedTable> {
    ternvale_vmm::build_acpi_offline(2).expect("offline tables")
}

#[test]
fn corrupting_an_sdt_breaks_only_its_checksum_byte() {
    let clean = tables();
    let mut tables = clean.clone();
    let hit = corrupt_checksum(&mut tables, "GTDT").expect("corrupt");
    assert_eq!(hit.signature, "GTDT");
    assert_eq!(hit.offset, 9);
    assert_eq!(hit.now, hit.was.wrapping_add(1));
    for (before, after) in clean.iter().zip(&tables) {
        if after.signature == "GTDT" {
            assert_eq!(after.gpa, hit.gpa);
            assert!(!after.checksum_ok());
            assert_eq!(ternvale_acpi::byte_sum(&after.bytes), 1);
            let changed: Vec<usize> = (0..after.bytes.len())
                .filter(|&i| before.bytes[i] != after.bytes[i])
                .collect();
            assert_eq!(changed, [9]);
        } else {
            assert_eq!(before, after);
        }
    }
}

#[test]
fn a_corrupted_set_is_written_with_its_bad_checksum_listed() {
    let mut tables = tables();
    corrupt_checksum(&mut tables, "GTDT").expect("corrupt");
    let dir = std::env::temp_dir().join(format!("ternvale-acpi-fault-{}", std::process::id()));
    crate::acpi::write_set(&dir, &tables).expect("write");
    let listing = std::fs::read_to_string(dir.join("tables.txt")).expect("tables.txt");
    let bad: Vec<&str> = listing.lines().filter(|l| l.contains("BAD")).collect();
    assert_eq!(bad.len(), 1, "{listing}");
    assert!(bad[0].trim_start().starts_with("GTDT"), "{listing}");
    let written = std::fs::read(dir.join("GTDT.dat")).expect("GTDT.dat");
    assert_eq!(ternvale_acpi::byte_sum(&written), 1);
    std::fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn the_rsdp_is_named_rsdp_and_uses_offset_8() {
    let mut tables = tables();
    let hit = corrupt_checksum(&mut tables, "RSDP").expect("corrupt");
    assert_eq!((hit.signature.as_str(), hit.offset), ("RSD PTR ", 8));
    assert!(!tables[0].checksum_ok());
}

#[test]
fn an_unknown_signature_is_an_error_listing_the_tables() {
    let mut tables = tables();
    let err = corrupt_checksum(&mut tables, "SSDT").expect_err("no SSDT");
    assert!(err.to_string().contains("no SSDT table"), "{err}");
    assert!(err.to_string().contains("DBG2"), "{err}");
    assert!(tables.iter().all(DumpedTable::checksum_ok));
}

#[test]
fn a_table_too_short_for_a_checksum_is_an_error() {
    let mut tables = vec![DumpedTable {
        signature: "GTDT".into(),
        gpa: 0x1000,
        bytes: vec![0; 4],
    }];
    assert!(corrupt_checksum(&mut tables, "GTDT").is_err());
}

#[test]
fn no_or_empty_spec_changes_nothing() {
    let clean = tables();
    let mut tables = clean.clone();
    assert_eq!(apply(&mut tables, None).expect("none"), None);
    assert_eq!(apply(&mut tables, Some("  ")).expect("empty"), None);
    assert_eq!(tables, clean);
}

#[cfg(not(feature = "acpi-fault-injection"))]
#[test]
fn without_the_feature_a_spec_is_ignored() {
    let clean = tables();
    let mut tables = clean.clone();
    assert_eq!(apply(&mut tables, Some("GTDT")).expect("ignored"), None);
    assert_eq!(tables, clean);
}

#[cfg(feature = "acpi-fault-injection")]
#[test]
fn with_the_feature_a_spec_corrupts_the_table() {
    let mut tables = tables();
    let hit = apply(&mut tables, Some("MCFG"))
        .expect("apply")
        .expect("corrupted");
    assert_eq!(hit.signature, "MCFG");
    let err = apply(&mut tables, Some("NOPE")).expect_err("unknown");
    assert!(
        format!("{err:#}").contains("TERNVALE_ACPI_CORRUPT=NOPE"),
        "{err:#}"
    );
}
