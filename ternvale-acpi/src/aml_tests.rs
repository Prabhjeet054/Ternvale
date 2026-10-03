use super::*;

/// Decode a PkgLength; returns (value, bytes used).
fn decode(bytes: &[u8]) -> (usize, usize) {
    let lead = bytes[0];
    let follow = usize::from(lead >> 6);
    if follow == 0 {
        return (usize::from(lead & 0x3f), 1);
    }
    let mut value = usize::from(lead & 0x0f);
    for index in 0..follow {
        value |= usize::from(bytes[1 + index]) << (4 + 8 * index);
    }
    (value, 1 + follow)
}

#[test]
fn short_packages_use_one_byte() {
    assert_eq!(pkg_length(0).unwrap(), [0x01]);
    assert_eq!(pkg_length(5).unwrap(), [0x06]);
    assert_eq!(pkg_length(62).unwrap(), [0x3f]);
}

#[test]
fn longer_packages_switch_encoding_at_each_boundary() {
    // 63 + 2 = 65 = 0x41: lead 0x40 | 0x1, then 0x41 >> 4.
    assert_eq!(pkg_length(63).unwrap(), [0x41, 0x04]);
    assert_eq!(pkg_length(0xfff - 2).unwrap().len(), 2);
    assert_eq!(pkg_length(0xfff - 1).unwrap().len(), 3);
    assert_eq!(pkg_length(0xf_ffff - 3).unwrap().len(), 3);
    assert_eq!(pkg_length(0xf_ffff - 2).unwrap().len(), 4);
    assert_eq!(pkg_length(MAX_PKG_LENGTH - 4).unwrap().len(), 4);
}

#[test]
fn pkg_length_counts_its_own_bytes() {
    for content in [
        0,
        1,
        61,
        62,
        63,
        64,
        300,
        4093,
        4094,
        70_000,
        1 << 20,
        MAX_PKG_LENGTH - 4,
    ] {
        let encoded = pkg_length(content).unwrap();
        let (value, used) = decode(&encoded);
        assert_eq!(used, encoded.len(), "content {content}");
        assert_eq!(value, content + used, "content {content}");
        if used > 1 {
            assert_eq!(
                encoded[0] & 0x30,
                0,
                "bits 5-4 must be zero, content {content}"
            );
        }
    }
}

#[test]
fn oversized_package_is_rejected() {
    let error = pkg_length(MAX_PKG_LENGTH - 3).unwrap_err();
    assert!(
        matches!(error, AcpiError::PkgLengthTooLarge { .. }),
        "{error}"
    );
    assert!(pkg_length(usize::MAX).is_err());
}

#[test]
fn name_segments_are_padded_and_validated() {
    assert_eq!(&name_seg("_SB").unwrap(), b"_SB_");
    assert_eq!(&name_seg("PCI0").unwrap(), b"PCI0");
    assert_eq!(&name_seg("A").unwrap(), b"A___");
    for bad in ["", "TOOLONG", "0ABC", "pci0", "A-B", "É"] {
        assert!(
            matches!(name_seg(bad), Err(AcpiError::BadNameSeg { .. })),
            "{bad:?} accepted"
        );
    }
}

#[test]
fn name_strings_use_the_right_prefixes() {
    assert_eq!(name_string("\\_SB").unwrap(), b"\\_SB_");
    assert_eq!(name_string("\\").unwrap(), [ROOT_CHAR, NULL_NAME]);
    assert_eq!(name_string("^^DEV").unwrap(), b"^^DEV_");
    assert_eq!(name_string("\\_SB.PCI0").unwrap(), b"\\\x2e_SB_PCI0");
    assert_eq!(name_string("A.B.C").unwrap(), b"\x2f\x03A___B___C___");
    assert!(matches!(
        name_string(""),
        Err(AcpiError::BadNamePath { .. })
    ));
    assert!(matches!(
        name_string("\\_SB..X"),
        Err(AcpiError::BadNameSeg { .. })
    ));
}

#[test]
fn empty_root_sb_scope_is_seven_bytes() {
    // ScopeOp, PkgLength 6 (itself + 5 name bytes), RootChar, "_SB_".
    assert_eq!(
        scope("\\_SB", &[]).unwrap(),
        [0x10, 0x06, 0x5c, b'_', b'S', b'B', b'_']
    );
}

#[test]
fn scope_length_covers_name_and_body() {
    let body = vec![0xa3; 100];
    let encoded = scope("\\_SB", &body).unwrap();
    assert_eq!(encoded[0], SCOPE_OP);
    let (value, used) = decode(&encoded[1..]);
    assert_eq!(value, used + 5 + body.len());
    assert_eq!(encoded.len(), 1 + value);
}
