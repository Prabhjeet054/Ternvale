use super::*;
use crate::config::tests::sample;

/// Bytes after `Name (seg, ` in `aml` (the first match).
fn named<'a>(aml: &'a [u8], seg: &[u8; 4]) -> &'a [u8] {
    let mut needle = vec![crate::aml::NAME_OP];
    needle.extend_from_slice(seg);
    let at = aml
        .windows(needle.len())
        .position(|w| w == needle.as_slice())
        .unwrap_or_else(|| panic!("no Name ({})", String::from_utf8_lossy(seg)));
    &aml[at + needle.len()..]
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn pci0_is_a_device_with_the_root_bridge_ids() {
    let aml = pci_root(&sample(1)).unwrap();
    assert_eq!(&aml[..2], &[0x5b, 0x82], "DeviceOp");
    let name_at = 2 + if aml[2] & 0xc0 == 0 {
        1
    } else {
        1 + (aml[2] >> 6) as usize
    };
    assert_eq!(&aml[name_at..name_at + 4], b"PCI0");
    assert_eq!(&named(&aml, b"_HID")[..5], &[0x0c, 0x41, 0xd0, 0x0a, 0x08]);
    assert_eq!(&named(&aml, b"_CID")[..5], &[0x0c, 0x41, 0xd0, 0x0a, 0x03]);
    assert_eq!(named(&aml, b"_UID")[0], 0x00, "_UID Zero");
    assert_eq!(named(&aml, b"_SEG")[0], 0x00, "_SEG Zero");
    assert_eq!(named(&aml, b"_BBN")[0], 0x00, "_BBN Zero");
    assert_eq!(named(&aml, b"_CCA")[0], 0x01, "_CCA One");
}

#[test]
fn crs_has_the_ecam_buses_and_the_mmio_window_and_no_io() {
    let config = sample(1);
    let aml = pci_root(&config).unwrap();
    let crs = named(&aml, b"_CRS");
    assert_eq!(crs[0], crate::aml_data::BUFFER_OP);
    let bus = word_bus_number(0, 15).unwrap();
    let mmio = dword_memory(Usage::Producer, 0x1000_0000, 0x2f00_0000).unwrap();
    // BufferOp, PkgLength, BytePrefix size, descriptors, End Tag.
    let size = usize::from(crs[3]);
    assert_eq!(size, bus.len() + mmio.len() + 2);
    let template = &crs[4..4 + size];
    assert_eq!(&template[..bus.len()], bus.as_slice());
    assert_eq!(
        &template[bus.len()..bus.len() + mmio.len()],
        mmio.as_slice()
    );
    assert_eq!(&template[size - 2..], &[0x79, 0x00]);
    assert_eq!(
        descriptor_tags(template),
        [0x88, 0x87, 0x79],
        "bus range, memory window, end; no I/O port or I/O range descriptor"
    );
    assert_eq!(
        template[bus.len() + 3],
        0,
        "the window is memory, not I/O (type 1)"
    );
}

/// Item tags in a resource template (ACPI 6.5 §6.4.1: bit 7 set = large
/// item with a 16-bit length, else small item with length in bits 2..0).
fn descriptor_tags(template: &[u8]) -> Vec<u8> {
    let mut tags = Vec::new();
    let mut at = 0;
    while at < template.len() {
        let tag = template[at];
        tags.push(tag);
        at += if tag & 0x80 != 0 {
            3 + usize::from(u16::from_le_bytes([template[at + 1], template[at + 2]]))
        } else {
            1 + usize::from(tag & 0x07)
        };
    }
    assert_eq!(at, template.len(), "descriptors fill the template exactly");
    tags
}

#[test]
fn res0_reserves_the_ecam_window_as_a_consumer() {
    let aml = pci_root(&sample(1)).unwrap();
    let res0 = aml
        .windows(6)
        .position(|w| w == [0x5b, 0x82, w[2], b'R', b'E', b'S'])
        .expect("Device (RES0)");
    let res0 = &aml[res0..];
    assert_eq!(&named(res0, b"_HID")[..5], &[0x0c, 0x41, 0xd0, 0x0c, 0x02]);
    let ecam = dword_memory(Usage::Consumer, 0x3f00_0000, 16 << 20).unwrap();
    assert!(contains(buffer(named(res0, b"_CRS")), &ecam));
    assert!(
        !contains(buffer(named(&aml, b"_CRS")), &ecam[6..]),
        "the root bridge _CRS does not claim the ECAM range"
    );
}

/// The Buffer at the start of `aml` (one-byte PkgLength).
fn buffer(aml: &[u8]) -> &[u8] {
    assert_eq!(aml[0], crate::aml_data::BUFFER_OP);
    assert_eq!(aml[1] & 0xc0, 0, "one-byte PkgLength");
    &aml[..1 + usize::from(aml[1])]
}

#[test]
fn prt_swizzles_every_device_and_pin_onto_the_four_gsivs() {
    let routes = prt_routes(&sample(1));
    assert_eq!(routes.len(), 128);
    let gsiv = |device: u8, pin: u8| {
        routes
            .iter()
            .find(|r| r.device == device && r.pin == pin)
            .expect("route")
            .gsiv
    };
    assert_eq!(gsiv(0, 0), 35, "00 INTA -> SPI 3");
    assert_eq!(gsiv(0, 3), 38, "00 INTD -> SPI 6");
    assert_eq!(gsiv(1, 0), 36, "01 INTA -> SPI 4");
    assert_eq!(gsiv(1, 3), 35, "01 INTD wraps to SPI 3");
    assert_eq!(gsiv(31, 1), 35, "(31 + 1) % 4 = 0");
    let aml = pci_root(&sample(1)).unwrap();
    let prt = named(&aml, b"_PRT");
    assert_eq!(prt[0], crate::aml_data::PACKAGE_OP);
    assert_eq!(prt[3], 128, "NumElements after a two-byte PkgLength");
    // Package (4) { 0x0001FFFF, 0x03, Zero, 0x23 }: device 1 INTD.
    assert!(contains(
        prt,
        &[0x12, 0x0c, 0x04, 0x0c, 0xff, 0xff, 0x01, 0x00, 0x0a, 0x03, 0x00, 0x0a, 0x23]
    ));
    // Package (4) { 0xFFFF, Zero, Zero, 0x23 }: device 0 INTA.
    assert!(contains(
        prt,
        &[0x12, 0x09, 0x04, 0x0b, 0xff, 0xff, 0x00, 0x00, 0x0a, 0x23]
    ));
}

#[test]
fn a_later_first_bus_moves_bbn_and_the_reservation() {
    let mut config = sample(1);
    config.ecam.start_bus = 2;
    config.ecam.end_bus = 3;
    let aml = pci_root(&config).unwrap();
    assert_eq!(&named(&aml, b"_BBN")[..2], &[0x0a, 0x02]);
    assert!(contains(&aml, &word_bus_number(2, 3).unwrap()));
    assert!(contains(
        &aml,
        &dword_memory(Usage::Consumer, 0x3f00_0000, 2 << 20).unwrap()
    ));
}
