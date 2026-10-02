//! BAR sizing (write all-ones, read back the mask) and capability-list walks,
//! both on `ConfigSpace` directly and through the ECAM window on an `MmioBus`.

use std::io::Write;
use std::sync::{Arc, Mutex};

use super::bus_tests::{setup, Guest};
use super::{
    register_name, Bar, BarKind, Capability, ConfigSpace, Header, PciError, PciFunction, CAP_PTR,
    COMMAND_MEMORY, STATUS, STATUS_CAP_LIST,
};

const ALL_ONES: u64 = 0xffff_ffff;

struct Fixed(ConfigSpace);

impl PciFunction for Fixed {
    fn name(&self) -> &str {
        "fixed"
    }

    fn config(&self) -> &ConfigSpace {
        &self.0
    }

    fn config_mut(&mut self) -> &mut ConfigSpace {
        &mut self.0
    }

    fn read_bar(&mut self, bar: usize, offset: u64, _size: u8) -> u64 {
        (bar as u64) << 16 | offset
    }
}

fn header() -> Header {
    Header {
        vendor_id: 0x1234,
        device_id: 0xabcd,
        class_code: 0xff_00_00,
        revision: 0,
        subsystem_vendor_id: 0,
        subsystem_id: 0,
        interrupt_pin: 1,
    }
}

fn bar(size: u64, kind: BarKind, prefetchable: bool) -> Bar {
    Bar {
        size,
        kind,
        prefetchable,
    }
}

/// BAR0 4 KiB mem32, BAR1 empty, BAR2-3 1 MiB mem64 prefetchable,
/// BAR4 64 KiB mem32 prefetchable, BAR5 empty.
fn mixed_bars() -> ConfigSpace {
    let mut config = ConfigSpace::new(header());
    config
        .add_bar(0, bar(0x1000, BarKind::Mem32, false))
        .expect("bar0");
    config
        .add_bar(2, bar(0x10_0000, BarKind::Mem64, true))
        .expect("bar2");
    config
        .add_bar(4, bar(0x1_0000, BarKind::Mem32, true))
        .expect("bar4");
    config
}

/// Vendor (4 bytes), MSI-X shaped (12 bytes), and MSI shaped (14 bytes).
fn three_caps(config: &mut ConfigSpace) -> Vec<u8> {
    vec![
        config.add_capability(0x09, &[4, 0], &[]).expect("vendor"),
        config.add_capability(0x11, &[0; 10], &[]).expect("msix"),
        config.add_capability(0x05, &[0; 12], &[]).expect("msi"),
    ]
}

/// Size decoded from the all-ones readback, as Linux's `__pci_read_base` does.
fn size_from_mask(low: u32, high: Option<u32>) -> u64 {
    let mask = u64::from(high.unwrap_or(u32::MAX)) << 32 | u64::from(low & !0xf);
    if low & !0xf == 0 && high.is_none_or(|h| h == 0) {
        return 0;
    }
    (!mask).wrapping_add(1)
}

/// Linux's sizing sequence for BAR `index` of `device`: decode off, save,
/// write all-ones, read the mask, restore, decode back on. Returns
/// `(mask, original)`.
fn size_via_ecam(guest: &Guest, device: u64, index: u64) -> (u32, u32) {
    let reg = 0x10 + 4 * index;
    let command = guest.cfg_read(device, 0x04, 2);
    guest.cfg_write(device, 0x04, 2, command & !u64::from(COMMAND_MEMORY));
    let original = guest.cfg_read(device, reg, 4) as u32;
    guest.cfg_write(device, reg, 4, ALL_ONES);
    let mask = guest.cfg_read(device, reg, 4) as u32;
    guest.cfg_write(device, reg, 4, u64::from(original));
    guest.cfg_write(device, 0x04, 2, command);
    (mask, original)
}

#[test]
fn all_ones_sizing_through_ecam_reads_back_each_bar_mask() {
    let (_g, root, _) = setup();
    root.add(|_, _| Ok(Box::new(Fixed(mixed_bars()))))
        .expect("add");
    let guest = Guest::attach(&root);
    guest.cfg_write(1, 0x04, 2, u64::from(COMMAND_MEMORY));

    let probed: Vec<(u32, u32)> = (0..6).map(|i| size_via_ecam(&guest, 1, i)).collect();
    let masks: Vec<u32> = probed.iter().map(|&(mask, _)| mask).collect();
    assert_eq!(
        masks,
        vec![
            0xffff_f000, // 4 KiB, mem32, non-prefetchable
            0,           // unimplemented
            0xfff0_000c, // 1 MiB low half: 64-bit (bit 2) + prefetchable (bit 3)
            0xffff_ffff, // upper half of a BAR below 4 GiB
            0xffff_0008, // 64 KiB, mem32, prefetchable
            0,           // unimplemented
        ]
    );
    assert_eq!(size_from_mask(masks[0], None), 0x1000);
    assert_eq!(size_from_mask(masks[1], None), 0);
    assert_eq!(size_from_mask(masks[2], Some(masks[3])), 0x10_0000);
    assert_eq!(size_from_mask(masks[4], None), 0x1_0000);

    // Restore put the host assignment back, and decode works again there.
    for (index, &(_, original)) in probed.iter().enumerate() {
        assert_eq!(
            guest.cfg_read(1, 0x10 + 4 * index as u64, 4) as u32,
            original
        );
    }
    let bar0 = u64::from(probed[0].1 & !0xf);
    assert_ne!(bar0, 0, "host pre-assigned bar0");
    assert_eq!(
        guest.read(bar0 + 0x24, 4),
        0x24,
        "bar0 routes after restore"
    );
    let bar4 = u64::from(probed[4].1 & !0xf);
    assert_eq!(
        guest.read(bar4 + 8, 4),
        4 << 16 | 8,
        "bar4 routes after restore"
    );
}

#[test]
fn mask_is_the_complement_of_the_size_for_every_power_of_two() {
    for shift in 4..=31 {
        let size = 1u64 << shift;
        for prefetchable in [false, true] {
            let mut config = ConfigSpace::new(header());
            config
                .add_bar(1, bar(size, BarKind::Mem32, prefetchable))
                .expect("mem32");
            config.write(0x14, 4, u32::MAX);
            let flags = if prefetchable { 0x8 } else { 0 };
            let mask = config.read(0x14, 4);
            assert_eq!(mask, !(size as u32 - 1) | flags, "mem32 size {size:#x}");
            assert_eq!(size_from_mask(mask, None), size);
        }
    }
    for shift in 4..=40 {
        let size = 1u64 << shift;
        let mut config = ConfigSpace::new(header());
        config
            .add_bar(0, bar(size, BarKind::Mem64, false))
            .expect("mem64");
        config.write(0x10, 4, u32::MAX);
        config.write(0x14, 4, u32::MAX);
        let (low, high) = (config.read(0x10, 4), config.read(0x14, 4));
        let address_mask = !(size - 1);
        assert_eq!(
            low,
            (address_mask as u32 & !0xf) | 0x4,
            "mem64 low {size:#x}"
        );
        assert_eq!(high, (address_mask >> 32) as u32, "mem64 high {size:#x}");
        assert_eq!(size_from_mask(low, Some(high)), size);
    }
}

#[test]
fn byte_word_and_zero_writes_keep_the_read_only_type_bits() {
    let mut config = mixed_bars();
    for byte in 0..4 {
        config.write(0x18 + byte, 1, 0xff);
    }
    assert_eq!(config.read(0x18, 4), 0xfff0_000c, "four byte writes");
    config.write(0x18, 4, 0);
    assert_eq!(config.read(0x18, 4), 0x0000_000c, "zero keeps the flags");
    config.write(0x18, 2, 0xffff);
    config.write(0x1a, 2, 0xffff);
    assert_eq!(config.read(0x18, 4), 0xfff0_000c, "two word writes");
    config.write(0x10, 4, 0x1234_5fff);
    assert_eq!(
        config.read(0x10, 4),
        0x1234_5000,
        "bits below the size drop"
    );
    assert_eq!(config.bar_address(0), Some(0x1234_5000));
}

#[test]
fn host_bridge_bars_rom_and_capabilities_are_empty() {
    let (guest, root, _) = setup();
    for reg in (0x10..=0x24).step_by(4).chain([0x30]) {
        guest.cfg_write(0, reg, 4, ALL_ONES);
        assert_eq!(guest.cfg_read(0, reg, 4), 0, "host bridge reg {reg:#x}");
    }
    assert_eq!(
        guest.cfg_read(0, STATUS as u64, 2) & u64::from(STATUS_CAP_LIST),
        0
    );
    assert_eq!(guest.cfg_read(0, CAP_PTR as u64, 1), 0);
    assert_eq!(
        root.config_write(super::Bdf::new(0), 0x10, 4, u32::MAX),
        Some(0)
    );
    assert_eq!(
        root.config_write(super::Bdf::new(5), 0x10, 4, u32::MAX),
        None
    );
}

/// Walk the list over ECAM the way Linux's `pci_find_capability` does.
fn walk_via_ecam(guest: &Guest, device: u64) -> Vec<(u64, u64)> {
    let mut caps = Vec::new();
    if guest.cfg_read(device, STATUS as u64, 2) & u64::from(STATUS_CAP_LIST) == 0 {
        return caps;
    }
    let mut at = guest.cfg_read(device, CAP_PTR as u64, 1) & !3;
    while at != 0 {
        assert!(caps.len() < 48, "capability walk did not terminate");
        caps.push((at, guest.cfg_read(device, at, 1)));
        at = guest.cfg_read(device, at + 1, 1) & !3;
    }
    caps
}

#[test]
fn capability_walk_through_ecam_matches_the_config_walk() {
    let mut config = ConfigSpace::new(header());
    let offsets = three_caps(&mut config);
    assert_eq!(offsets, vec![0x40, 0x44, 0x50], "dword aligned, in order");
    let walked = config.capabilities().expect("walk");
    assert_eq!(
        walked,
        vec![
            Capability {
                offset: 0x40,
                id: 0x09,
                next: 0x44
            },
            Capability {
                offset: 0x44,
                id: 0x11,
                next: 0x50
            },
            Capability {
                offset: 0x50,
                id: 0x05,
                next: 0
            },
        ]
    );

    let (_g, root, _) = setup();
    root.add(move |_, _| Ok(Box::new(Fixed(config))))
        .expect("add");
    let guest = Guest::attach(&root);
    assert_eq!(
        walk_via_ecam(&guest, 1),
        vec![(0x40, 0x09), (0x44, 0x11), (0x50, 0x05)]
    );
    assert!(
        walk_via_ecam(&guest, 0).is_empty(),
        "host bridge has no list"
    );
}

#[test]
fn walk_masks_reserved_bits_and_ignores_a_cleared_status_bit() {
    let mut config = ConfigSpace::new(header());
    three_caps(&mut config);
    config.set_bytes(CAP_PTR, &[0x43]);
    assert_eq!(config.capabilities().expect("walk").len(), 3, "0x43 & !3");
    let status = config.u16_at(STATUS) & !STATUS_CAP_LIST;
    config.set_bytes(STATUS, &status.to_le_bytes());
    assert!(config.capabilities().expect("walk").is_empty());
}

#[test]
fn walk_reports_loops_and_pointers_into_the_header() {
    let mut looped = ConfigSpace::new(header());
    three_caps(&mut looped);
    looped.set_bytes(0x51, &[0x44]);
    assert!(matches!(
        looped.capabilities(),
        Err(PciError::CapabilityList { offset: 0x44, .. })
    ));

    let mut into_header = ConfigSpace::new(header());
    three_caps(&mut into_header);
    into_header.set_bytes(0x45, &[0x10]);
    assert!(matches!(
        into_header.capabilities(),
        Err(PciError::CapabilityList { offset: 0x10, .. })
    ));

    let (_g, root, _) = setup();
    let err = root
        .add(move |_, _| Ok(Box::new(Fixed(looped))))
        .unwrap_err();
    assert!(matches!(err, PciError::CapabilityList { .. }), "{err}");
    assert_eq!(
        root.function_count(),
        1,
        "rejected function is not on the bus"
    );
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("captured").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn every_ecam_access_is_logged_at_trace_with_register_names() {
    // With a single registered dispatcher, tracing caches a callsite's interest
    // from whichever thread hits it first; parallel tests have no subscriber, so
    // the pci callsites would be cached as disabled. A second live dispatcher
    // makes the interest the union over all of them.
    let _second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let (_g, root, _) = setup();
        root.add(|_, _| Ok(Box::new(Fixed(mixed_bars()))))
            .expect("add");
        let guest = Guest::attach(&root);
        guest.cfg_read(1, 0x00, 4);
        guest.cfg_write(1, 0x10, 4, ALL_ONES);
        guest.cfg_read(1, 0x10, 4);
        guest.cfg_read(7, 0x00, 2);
    });
    let text = String::from_utf8(captured.0.lock().expect("captured").clone()).expect("utf8");
    let lines: Vec<&str> = text.lines().filter(|l| l.contains("pci config")).collect();
    assert_eq!(lines.len(), 4, "{text}");
    for (line, want) in lines.iter().zip([
        "addr=\"0x3f008000\" bdf=00:01.0 reg=\"0x0\" field=\"vendor_id\" size=4 value=\"0xabcd1234\" present=true direction=\"read\"",
        "addr=\"0x3f008010\" bdf=00:01.0 reg=\"0x10\" field=\"bar0\" size=4 value=\"0xffffffff\" stored=Some(\"0xfffff000\") present=true direction=\"write\"",
        "addr=\"0x3f008010\" bdf=00:01.0 reg=\"0x10\" field=\"bar0\" size=4 value=\"0xfffff000\" present=true direction=\"read\"",
        "addr=\"0x3f038000\" bdf=00:07.0 reg=\"0x0\" field=\"vendor_id\" size=2 value=\"0xffff\" present=false direction=\"read\"",
    ]) {
        assert!(line.contains("TRACE") && line.contains(want), "{line}\nwant {want}");
    }
}

#[test]
fn register_names_cover_the_type0_header() {
    assert_eq!(register_name(0x00), "vendor_id");
    assert_eq!(register_name(0x04), "command");
    assert_eq!(register_name(0x06), "status");
    assert_eq!(register_name(0x0e), "header_type");
    assert_eq!(register_name(0x1c), "bar3");
    assert_eq!(register_name(0x24), "bar5");
    assert_eq!(register_name(0x30), "expansion_rom");
    assert_eq!(register_name(0x34), "cap_ptr");
    assert_eq!(register_name(0x3d), "interrupt_pin");
    assert_eq!(register_name(0x40), "capability");
    assert_eq!(register_name(0xff), "capability");
    assert_eq!(register_name(0x100), "extended");
}
