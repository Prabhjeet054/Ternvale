use super::*;
use crate::config::tests::sample;
use crate::sdt::{checksum, SDT_CHECKSUM_OFFSET};
use crate::{dbg2, gtdt, madt, mcfg, spcr};

/// Rewrite the length field and checksum after a test edits a table.
fn refit(bytes: &mut Vec<u8>, len: usize) {
    bytes.truncate(len);
    bytes[4..8].copy_from_slice(&(len as u32).to_le_bytes());
    bytes[SDT_CHECKSUM_OFFSET] = 0;
    bytes[SDT_CHECKSUM_OFFSET] = checksum(bytes);
}

#[test]
fn builders_decode_back_to_their_config() {
    let config = sample(2);
    let info = decode_madt(&madt(&config).expect("madt")).expect("decode");
    assert_eq!(info.gicds, vec![(0x0800_0000, 3)]);
    assert_eq!(info.gicrs, vec![(0x080a_0000, 0x00f6_0000)]);
    let mpidrs: Vec<_> = info
        .giccs
        .iter()
        .map(|g| (g.uid, g.mpidr, g.flags))
        .collect();
    assert_eq!(mpidrs, vec![(0, 0, 1), (1, 1, 1)]);

    let gtdt = decode_gtdt(&gtdt(&config.timer).expect("gtdt")).expect("decode");
    assert_eq!(gtdt.gsivs, [29, 30, 27, 26]);
    assert_eq!(gtdt.flags, [4; 4]);

    let mcfg = decode_mcfg(&mcfg(&config.ecam).expect("mcfg")).expect("decode");
    assert_eq!(
        mcfg,
        vec![McfgAllocation {
            base: 0x3f00_0000,
            segment: 0,
            start_bus: 0,
            end_bus: 15
        }]
    );

    let spcr = decode_spcr(&spcr(&config.uart).expect("spcr")).expect("decode");
    assert_eq!(spcr.interface_type, 3);
    assert_eq!(spcr.base, Gas::mmio32(0x0900_0000));
    assert_eq!((spcr.interrupt_type, spcr.gsiv), (8, 33));

    let dbg2 = decode_dbg2(&dbg2(&config.uart).expect("dbg2")).expect("decode");
    assert_eq!(
        dbg2,
        vec![Dbg2Device {
            port_type: 0x8000,
            subtype: 3,
            base: Gas::mmio32(0x0900_0000),
            size: 0x1000
        }]
    );
}

#[test]
fn wrong_signature_and_bad_lengths_are_errors() {
    let gtdt = gtdt(&sample(1).timer).expect("gtdt");
    assert!(decode_madt(&gtdt)
        .unwrap_err()
        .to_string()
        .contains("signature"));
    assert!(decode_gtdt(&gtdt[..100])
        .unwrap_err()
        .to_string()
        .contains("length"));
    let mut short = gtdt.clone();
    refit(&mut short, 60);
    assert!(
        decode_gtdt(&short).is_err(),
        "timers past the header length"
    );
}

#[test]
fn malformed_madt_structures_do_not_loop_or_panic() {
    let good = madt(&sample(1)).expect("madt");
    let first = SDT_HEADER_LEN + MADT_FIXED_LEN;
    for len in [0u8, 1, 255] {
        let mut bad = good.clone();
        bad[first + 1] = len;
        let full = bad.len();
        refit(&mut bad, full);
        assert!(decode_madt(&bad).is_err(), "structure length {len}");
    }
    let mut cut = good.clone();
    refit(&mut cut, first + 40);
    assert!(decode_madt(&cut).is_err(), "gicc cut mid-structure");
}

#[test]
fn malformed_dbg2_is_an_error() {
    let good = dbg2(&sample(1).uart).expect("dbg2");
    let mut far = good.clone();
    far[36..40].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
    let full = far.len();
    refit(&mut far, full);
    assert!(decode_dbg2(&far).is_err(), "device offset past the table");
    let mut zero = good.clone();
    zero[45..47].copy_from_slice(&0u16.to_le_bytes());
    refit(&mut zero, full);
    assert!(decode_dbg2(&zero).is_err(), "zero-length device");
    let mut gas = good;
    gas[44 + 18..44 + 20].copy_from_slice(&200u16.to_le_bytes());
    refit(&mut gas, full);
    assert!(
        decode_dbg2(&gas).is_err(),
        "register offset past the device"
    );
}
