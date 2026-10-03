//! Checksum correctness for the RSDP, XSDT, and FADT: the sum of all bytes
//! modulo 256 is zero (ACPI 6.5 §5.2.5.3 for both RSDP checksums, §5.2.6 for
//! SDTs). The sum here is computed independently of `sdt::byte_sum`.

use crate::{fadt, rsdp, xsdt, AcpiTables, PsciConduit};

fn sum_mod_256(bytes: &[u8]) -> u32 {
    bytes.iter().map(|byte| u32::from(*byte)).sum::<u32>() % 256
}

/// Addresses that stress every byte lane of a 64-bit pointer.
const ADDRESSES: [u64; 7] = [
    0,
    1,
    0x0910_0028,
    0x4000_0000,
    0x1234_5678_9abc_def0,
    0x8000_0000_0000_0000,
    u64::MAX,
];

/// Deterministic pseudo-random pointers (64-bit LCG, Knuth MMIX constants).
fn pointers(count: usize, seed: u64) -> Vec<u64> {
    let mut state = seed;
    (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state
        })
        .collect()
}

#[test]
fn rsdp_first_20_bytes_and_all_36_bytes_sum_to_zero() {
    for xsdt_gpa in ADDRESSES.into_iter().chain(pointers(64, 1)) {
        let bytes = rsdp(xsdt_gpa);
        assert_eq!(
            sum_mod_256(&bytes[..20]),
            0,
            "v1 checksum, xsdt {xsdt_gpa:#x}"
        );
        assert_eq!(
            sum_mod_256(&bytes),
            0,
            "extended checksum, xsdt {xsdt_gpa:#x}"
        );
    }
}

#[test]
fn xsdt_sums_to_zero_for_any_entry_list() {
    for count in 0..=32 {
        let entries = pointers(count, count as u64 + 7);
        let bytes = xsdt(&entries).expect("xsdt");
        assert_eq!(bytes.len(), 36 + 8 * count);
        assert_eq!(sum_mod_256(&bytes), 0, "{count} entries");
    }
    let bytes = xsdt(&ADDRESSES).expect("xsdt");
    assert_eq!(sum_mod_256(&bytes), 0, "edge addresses");
}

#[test]
fn fadt_sums_to_zero_for_both_conduits_and_any_dsdt() {
    for conduit in [PsciConduit::Hvc, PsciConduit::Smc] {
        for dsdt_gpa in ADDRESSES.into_iter().chain(pointers(64, 3)) {
            let bytes = fadt(dsdt_gpa, conduit).expect("fadt");
            assert_eq!(bytes.len(), 276);
            assert_eq!(sum_mod_256(&bytes), 0, "{conduit:?} dsdt {dsdt_gpa:#x}");
        }
    }
}

#[test]
fn laid_out_tables_sum_to_zero_at_any_base() {
    for base in [
        0,
        0x0910_0000,
        0x4000_0000,
        0x1_0000_0000,
        u64::MAX - 0xffff,
    ] {
        let tables = AcpiTables::build(base, 0x1_0000, PsciConduit::Hvc).expect("build");
        for table in tables.tables() {
            assert_eq!(
                sum_mod_256(&table.bytes),
                0,
                "{} at base {base:#x}",
                table.signature
            );
        }
        let rsdp = &tables.tables()[0].bytes;
        assert_eq!(sum_mod_256(&rsdp[..20]), 0, "rsdp v1 at base {base:#x}");
    }
}

#[test]
fn any_single_corrupted_byte_breaks_the_sum() {
    let tables = AcpiTables::build(0x0910_0000, 0x2_0000, PsciConduit::Hvc).expect("build");
    for table in tables.tables() {
        for at in 0..table.bytes.len() {
            let mut bytes = table.bytes.clone();
            bytes[at] = bytes[at].wrapping_add(1);
            assert_ne!(sum_mod_256(&bytes), 0, "{} byte {at}", table.signature);
        }
    }
}
