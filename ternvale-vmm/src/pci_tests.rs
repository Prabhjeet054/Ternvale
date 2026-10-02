use std::sync::{Arc, Mutex};

use super::{
    swizzle, Bar, BarKind, Bdf, ConfigSpace, Header, IntxLine, IntxPin, Msix, PciError, CAP_PTR,
    COMMAND, COMMAND_MEMORY, MSIX_CAP_ID, STATUS, STATUS_CAP_LIST,
};

fn header() -> Header {
    Header {
        vendor_id: 0x1af4,
        device_id: 0x1042,
        class_code: 0x01_00_00,
        revision: 1,
        subsystem_vendor_id: 0x1af4,
        subsystem_id: 0x0040,
        interrupt_pin: 1,
    }
}

#[test]
fn header_fields_are_little_endian_and_read_only() {
    let mut config = ConfigSpace::new(header());
    assert_eq!(config.read(0x00, 4), 0x1042_1af4);
    assert_eq!(
        config.read(0x08, 4),
        0x0100_0001,
        "class 0x010000, revision 1"
    );
    assert_eq!(config.read(0x2c, 4), 0x0040_1af4);
    assert_eq!(config.read(0x3d, 1), 1, "INTA");
    config.write(0x00, 4, 0xdead_beef);
    assert_eq!(config.read(0x00, 4), 0x1042_1af4, "IDs ignore writes");
    assert_eq!(config.read(0x0e, 1), 0, "single-function type 0");
}

#[test]
fn command_register_keeps_only_supported_bits() {
    let mut config = ConfigSpace::new(header());
    config.write(COMMAND as u16, 2, 0xffff);
    assert_eq!(config.command(), 0x0546);
    assert!(config.memory_enabled());
    assert!(config.intx_disabled());
    config.write(COMMAND as u16, 2, 0);
    assert_eq!(config.command(), 0);
}

#[test]
fn mem32_bar_sizing_returns_size_mask_and_flags() {
    let mut config = ConfigSpace::new(header());
    let bar = Bar {
        size: 0x4000,
        kind: BarKind::Mem32,
        prefetchable: false,
    };
    config.add_bar(0, bar).expect("bar0");
    config.write(0x10, 4, 0xffff_ffff);
    assert_eq!(config.read(0x10, 4), 0xffff_c000);
    config.write(0x10, 4, 0x1000_0000);
    assert_eq!(config.bar_address(0), Some(0x1000_0000));
    assert!(config.decoded_bars().is_empty(), "memory decode is off");
    config.write(COMMAND as u16, 2, u32::from(COMMAND_MEMORY));
    assert_eq!(config.decoded_bars(), vec![(0, 0x1000_0000, 0x4000)]);
    assert_eq!(config.read(0x14, 4), 0, "unimplemented bar reads 0");
    config.write(0x14, 4, 0xffff_ffff);
    assert_eq!(config.read(0x14, 4), 0, "unimplemented bar sizes to 0");
    config.write(0x30, 4, 0xffff_ffff);
    assert_eq!(config.read(0x30, 4), 0, "no expansion rom");
}

#[test]
fn mem64_prefetchable_bar_sizes_both_halves() {
    let mut config = ConfigSpace::new(header());
    let bar = Bar {
        size: 0x1_0000_0000,
        kind: BarKind::Mem64,
        prefetchable: true,
    };
    config.add_bar(2, bar).expect("bar2");
    config.write(0x18, 4, 0xffff_ffff);
    config.write(0x1c, 4, 0xffff_ffff);
    assert_eq!(
        config.read(0x18, 4),
        0x0000_000c,
        "64-bit prefetchable flags"
    );
    assert_eq!(config.read(0x1c, 4), 0xffff_ffff);
    config.set_bar_address(2, 0x80_0000_0000);
    assert_eq!(config.bar_address(2), Some(0x80_0000_0000));
    assert!(matches!(
        config.add_bar(3, bar),
        Err(PciError::BarTaken { index: 3 })
    ));
}

#[test]
fn bad_bars_are_rejected() {
    let mut config = ConfigSpace::new(header());
    let odd = Bar {
        size: 0x3000,
        kind: BarKind::Mem32,
        prefetchable: false,
    };
    assert!(matches!(
        config.add_bar(0, odd),
        Err(PciError::BarSize { .. })
    ));
    let wide = Bar {
        size: 0x1000,
        kind: BarKind::Mem64,
        prefetchable: false,
    };
    assert!(matches!(
        config.add_bar(5, wide),
        Err(PciError::BarIndex { .. })
    ));
    assert!(matches!(
        config.add_bar(6, odd),
        Err(PciError::BarIndex { .. })
    ));
}

#[test]
fn capability_list_links_in_order() {
    let mut config = ConfigSpace::new(header());
    assert_eq!(config.u16_at(STATUS) & STATUS_CAP_LIST, 0);
    let first = config
        .add_capability(0x09, &[14, 1, 0, 0], &[])
        .expect("cap");
    let second = config
        .add_capability(0x09, &[14, 2, 0, 0, 0xaa], &[0, 0, 0, 0, 0xff])
        .expect("cap");
    assert_eq!(config.u16_at(STATUS) & STATUS_CAP_LIST, STATUS_CAP_LIST);
    assert_eq!(config.read(CAP_PTR as u16, 1), u32::from(first));
    assert_eq!(first, 0x40);
    assert_eq!(config.read(u16::from(first) + 1, 1), u32::from(second));
    assert_eq!(second, 0x48, "dword aligned after a 6-byte capability");
    assert_eq!(config.read(u16::from(second) + 1, 1), 0, "end of list");
    config.write(u16::from(second) + 6, 1, 0x55);
    assert_eq!(
        config.read(u16::from(second) + 6, 1),
        0x55,
        "writable body byte"
    );
    config.write(u16::from(second) + 2, 1, 0x55);
    assert_eq!(
        config.read(u16::from(second) + 2, 1),
        14,
        "read-only body byte"
    );
    assert_eq!(config.read(0x100, 4), 0, "no extended capabilities");
    let big = vec![0u8; 250];
    assert!(matches!(
        config.add_capability(0x09, &big, &[]),
        Err(PciError::CapabilitySpace { .. })
    ));
}

#[test]
fn swizzle_matches_the_standard_interrupt_map() {
    assert_eq!(swizzle(0, 1), 0);
    assert_eq!(swizzle(1, 1), 1);
    assert_eq!(swizzle(3, 1), 3);
    assert_eq!(swizzle(4, 1), 0);
    assert_eq!(swizzle(1, 4), 0);
    assert_eq!(swizzle(2, 3), 0);
}

#[test]
fn shared_intx_line_stays_high_until_every_pin_drops() {
    let levels = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&levels);
    let line = IntxLine::new(
        4,
        Arc::new(move |level| seen.lock().expect("levels").push(level)),
    );
    let a = IntxPin::new("a".into(), Arc::clone(&line));
    let b = IntxPin::new("b".into(), line);
    a.set(true);
    b.set(true);
    a.set(false);
    assert_eq!(*levels.lock().expect("levels"), vec![true]);
    b.set(false);
    assert_eq!(*levels.lock().expect("levels"), vec![true, false]);
    b.set_disabled(true);
    b.set(true);
    assert!(
        b.asserted(),
        "status reflects the request even while disabled"
    );
    assert_eq!(
        levels.lock().expect("levels").len(),
        2,
        "disabled pin is silent"
    );
    b.set_disabled(false);
    assert_eq!(*levels.lock().expect("levels"), vec![true, false, true]);
    b.refresh(&|| false);
    assert_eq!(
        *levels.lock().expect("levels"),
        vec![true, false, true, false]
    );
    assert_eq!(b.spi(), 4);
}

#[test]
fn msix_stub_models_capability_and_table() {
    let mut config = ConfigSpace::new(header());
    let mut msix = Msix::new("t".into(), 3, 0, 0x4000, 0x5000);
    let cap = msix.add_capability(&mut config).expect("msix cap");
    assert_eq!(config.read(u16::from(cap), 1), u32::from(MSIX_CAP_ID));
    assert_eq!(
        config.u16_at(usize::from(cap) + 2) & 0x7ff,
        2,
        "table size - 1"
    );
    assert_eq!(config.u32_at(usize::from(cap) + 4), 0x4000);
    assert_eq!(config.u32_at(usize::from(cap) + 8), 0x5000);
    config.write(u16::from(cap) + 2, 2, 0xffff);
    assert_eq!(
        config.u16_at(usize::from(cap) + 2),
        0xc002,
        "only enable and mask are writable"
    );
    msix.sync(&config);
    assert!(msix.enabled());
    assert!(msix.claims(0, 0x4010));
    assert!(msix.claims(0, 0x5000));
    assert!(!msix.claims(0, 0x4030));
    assert_eq!(msix.read(0x400c, 4), 1, "vectors start masked");
    msix.write(0x4010, 8, 0x0000_0000_0800_0040);
    msix.write(0x4018, 4, 42);
    assert_eq!(msix.read(0x4010, 8), 0x0800_0040);
    assert_eq!(msix.read(0x4018, 4), 42);
    assert_eq!(msix.read(0x5000, 8), 0, "nothing pending");
    assert!(!msix.signal(1), "delivery is a stub");
}

#[test]
fn bdf_formats_like_lspci() {
    let bdf = Bdf::new(3);
    assert_eq!(bdf.to_string(), "00:03.0");
    assert_eq!(bdf.devfn(), 0x18);
}
