use super::*;
use crate::config::tests::sample;
use crate::sdt::{checksum, SDT_CHECKSUM_OFFSET};
use crate::tables::{AcpiTables, SIGNATURES};

const BASE: u64 = 0x4000_0000;
const LEN: usize = 0x4000;

/// A RAM image from `BASE` with Ternvale's tables at `BASE + 0x1000`.
fn image() -> (Vec<u8>, AcpiTables) {
    let tables = AcpiTables::build(BASE + 0x1000, 0x1000, &sample(1)).expect("build");
    let mut image = vec![0u8; LEN];
    for table in tables.tables() {
        let at = (table.gpa - BASE) as usize;
        image[at..at + table.bytes.len()].copy_from_slice(&table.bytes);
    }
    (image, tables)
}

fn rsdp_gpa(tables: &AcpiTables) -> u64 {
    tables.rsdp_gpa()
}

fn reader(image: &[u8]) -> impl FnMut(u64, usize) -> Result<Vec<u8>, String> + '_ {
    move |gpa, len| {
        let at = (gpa - BASE) as usize;
        image
            .get(at..at + len)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| "out of range".to_string())
    }
}

fn reseal(table: &mut [u8]) {
    table[SDT_CHECKSUM_OFFSET] = 0;
    table[SDT_CHECKSUM_OFFSET] = checksum(table);
}

#[test]
fn walk_returns_what_build_wrote() {
    let (image, tables) = image();
    let dumped = walk(rsdp_gpa(&tables), reader(&image)).expect("walk");
    let names: Vec<_> = dumped.iter().map(|t| t.signature.as_str()).collect();
    assert_eq!(names, SIGNATURES);
    assert_eq!(dumped.len(), tables.tables().len());
    for (dumped, built) in dumped.iter().zip(tables.tables()) {
        assert_eq!(dumped.gpa, built.gpa);
        assert_eq!(dumped.bytes, built.bytes);
        assert!(dumped.checksum_ok(), "{}", dumped.signature);
    }
    assert_eq!(dumped[0].revision(), 2);
    assert_eq!(dumped[2].revision(), 6);
    assert_eq!(dumped[0].file_name(), "RSDP.dat");
    assert_eq!(dumped[2].file_name(), "FACP.dat");
}

#[test]
fn image_scan_finds_an_unaligned_rsdp_and_skips_bad_candidates() {
    let (mut image, tables) = image();
    // A decoy with the signature but no valid checksum.
    image[0x10..0x18].copy_from_slice(b"RSD PTR ");
    assert_eq!(find_rsdp(&image, BASE), [rsdp_gpa(&tables)]);
    // A valid copy at an odd offset (EDK2 only 8-byte aligns its RSDP).
    let rsdp = tables.tables()[0].bytes.clone();
    image[0x203..0x203 + rsdp.len()].copy_from_slice(&rsdp);
    assert_eq!(find_rsdp(&image, BASE), [BASE + 0x203, rsdp_gpa(&tables)]);
    let dumped = walk_image(&image, BASE).expect("walk image");
    assert_eq!(dumped.len(), SIGNATURES.len());
    assert_eq!(dumped[0].gpa, BASE + 0x203);
    assert_eq!(dumped[1].gpa, tables.tables()[1].gpa);
}

#[test]
fn image_without_rsdp_is_an_error() {
    let error = walk_image(&[0u8; 256], BASE).unwrap_err();
    assert_eq!(
        error,
        AcpiError::NoRsdp {
            base: BASE,
            len: 256
        }
    );
}

#[test]
fn dsdt_falls_back_to_the_32_bit_pointer() {
    let (mut image, tables) = image();
    let fadt = &tables.tables()[2];
    let dsdt = &tables.tables()[3];
    let at = (fadt.gpa - BASE) as usize;
    let table = &mut image[at..at + fadt.bytes.len()];
    table[X_DSDT_OFFSET..X_DSDT_OFFSET + 8].fill(0);
    table[DSDT_OFFSET..DSDT_OFFSET + 4].copy_from_slice(&(dsdt.gpa as u32).to_le_bytes());
    reseal(table);
    let dumped = walk(rsdp_gpa(&tables), reader(&image)).expect("walk");
    assert_eq!(dumped[3].signature, "DSDT");
    assert_eq!(dumped[3].gpa, dsdt.gpa);
}

#[test]
fn bad_checksum_is_reported_not_fatal() {
    let (mut image, tables) = image();
    let fadt_at = (tables.tables()[2].gpa - BASE) as usize;
    image[fadt_at + 200] ^= 0xff;
    let dumped = walk(rsdp_gpa(&tables), reader(&image)).expect("walk");
    assert!(!dumped[2].checksum_ok());
    assert!(dumped[1].checksum_ok());
}

#[test]
fn guest_controlled_lengths_and_pointers_are_bounded() {
    let (image, tables) = image();
    let xsdt_at = (tables.tables()[1].gpa - BASE) as usize;

    let mut huge = image.clone();
    huge[xsdt_at + 4..xsdt_at + 8].copy_from_slice(&(MAX_TABLE_LEN + 1).to_le_bytes());
    let error = walk(rsdp_gpa(&tables), reader(&huge)).unwrap_err();
    assert!(
        matches!(error, AcpiError::BadTable { ref reason, .. } if reason.contains("length")),
        "{error}"
    );

    let mut tiny = image.clone();
    tiny[xsdt_at + 4..xsdt_at + 8].copy_from_slice(&4u32.to_le_bytes());
    assert!(matches!(
        walk(rsdp_gpa(&tables), reader(&tiny)),
        Err(AcpiError::BadTable { .. })
    ));

    let mut wild = image.clone();
    let entry = xsdt_at + SDT_HEADER_LEN;
    wild[entry..entry + 8].copy_from_slice(&0xdead_0000u64.to_le_bytes());
    reseal(&mut wild[xsdt_at..xsdt_at + tables.tables()[1].bytes.len()]);
    let error = walk(rsdp_gpa(&tables), |gpa: u64, len: usize| {
        gpa.checked_sub(BASE)
            .and_then(|at| image_get(&wild, at as usize, len))
            .ok_or("out of range")
    })
    .unwrap_err();
    assert!(
        matches!(
            error,
            AcpiError::Read {
                gpa: 0xdead_0000,
                ..
            }
        ),
        "{error}"
    );

    let error = walk(rsdp_gpa(&tables), |_, _| Ok::<_, String>(vec![0; 3])).unwrap_err();
    assert!(error.to_string().contains("3 of 36"), "{error}");
}

fn image_get(image: &[u8], at: usize, len: usize) -> Option<Vec<u8>> {
    image.get(at..at.checked_add(len)?).map(<[u8]>::to_vec)
}

#[test]
fn rsdp_problems_are_named() {
    let (image, tables) = image();
    let rsdp_at = (rsdp_gpa(&tables) - BASE) as usize;

    let error = walk(rsdp_gpa(&tables) + 1, reader(&image)).unwrap_err();
    assert!(error.to_string().contains("signature"), "{error}");

    let mut broken = image.clone();
    broken[rsdp_at + 20] ^= 1;
    let error = walk(rsdp_gpa(&tables), reader(&broken)).unwrap_err();
    assert!(error.to_string().contains("checksum"), "{error}");
}
