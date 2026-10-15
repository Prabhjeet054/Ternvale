//! `PciIdentity`: read back through config space, and formatted as Windows
//! hardware IDs and EDK2's `PciBus` line.

use std::sync::Arc;

use super::{Bdf, LineHook, PciIdentity, PciRoot, PCI_INTX_LINES};

fn root() -> Arc<PciRoot> {
    let hooks: [LineHook; PCI_INTX_LINES] = std::array::from_fn(|_| Arc::new(|_| {}) as LineHook);
    PciRoot::new(hooks)
}

#[test]
fn host_bridge_identity_matches_what_debug_edk2_printed() {
    let root = root();
    let bridge = PciIdentity::read(&root, Bdf::new(0)).expect("host bridge");
    assert_eq!(bridge.to_string(), "1b36:0008");
    assert_eq!(bridge.class_code, 0x06_00_00);
    // Verbatim from target/boot-logs/20261013-193923-firmware-debug/guest-serial.log.
    assert_eq!(
        bridge.edk2_discovered(),
        "PciBus: Discovered PCI @ [00|00|00]  [VID = 0x1B36, DID = 0x0008]"
    );
    assert_eq!(
        bridge.windows_hardware_id(),
        "PCI\\VEN_1B36&DEV_0008&SUBSYS_11001B36&REV_00"
    );
    assert_eq!(bridge.windows_compatible_id(), "PCI\\CC_060000");
    assert_eq!(PciIdentity::read(&root, Bdf::new(5)), None, "empty slot");
}

#[test]
fn virtio_blk_formats_as_the_ids_viostor_matches() {
    let blk = PciIdentity {
        bdf: Bdf::new(0x0a),
        vendor_id: 0x1af4,
        device_id: 0x1042,
        revision: 1,
        class_code: 0x01_00_00,
        subsystem_vendor_id: 0x1af4,
        subsystem_id: 0x1100,
    };
    assert_eq!(
        blk.windows_hardware_id(),
        "PCI\\VEN_1AF4&DEV_1042&SUBSYS_11001AF4&REV_01"
    );
    assert_eq!(blk.windows_compatible_id(), "PCI\\CC_010000");
    assert_eq!(
        blk.edk2_discovered(),
        "PciBus: Discovered PCI @ [00|0A|00]  [VID = 0x1AF4, DID = 0x1042]"
    );
}
