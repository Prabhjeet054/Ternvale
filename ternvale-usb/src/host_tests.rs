//! Unit tests for host USB helpers.

use super::host::UsbControlSetup;

#[test]
fn control_setup_round_trip() {
    let setup = UsbControlSetup {
        request_type: 0x80,
        request: 0x06,
        value: 0x0100,
        index: 0,
        length: 18,
    };
    let bytes = setup.to_bytes();
    let parsed = UsbControlSetup::from_bytes(&bytes).expect("parse");
    assert_eq!(parsed, setup);
}
