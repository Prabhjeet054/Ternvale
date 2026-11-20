use super::*;
use super::{FRAME_FLAG_PARTIAL, VERSION};

fn header() -> FrameHeader {
    FrameHeader {
        width: 640,
        height: 480,
        stride: 2560,
        format: 2,
        sequence: 0x1122_3344_5566_7788,
        payload_len: 2560 * 480,
        flags: 0,
        scanout: 0,
        damage_x: 0,
        damage_y: 0,
        damage_w: 640,
        damage_h: 480,
        scale_x100: 100,
    }
}

#[test]
fn frame_header_round_trips_little_endian() {
    let bytes = header().encode();
    assert_eq!(&bytes[0..4], b"TVDP");
    assert_eq!(&bytes[4..8], &[4, 0, 1, 0], "version 4, kind FRAME");
    assert_eq!(&bytes[8..12], &640u32.to_le_bytes());
    assert_eq!(&bytes[24..32], &0x1122_3344_5566_7788u64.to_le_bytes());
    assert_eq!(&bytes[36..40], &0u32.to_le_bytes(), "flags");
    assert_eq!(FrameHeader::decode(&bytes), Ok(header()));
}

#[test]
fn partial_damage_payload_is_packed_rect() {
    let header = FrameHeader {
        flags: FRAME_FLAG_PARTIAL,
        payload_len: 4 * 3 * 4,
        damage_x: 10,
        damage_y: 5,
        damage_w: 4,
        damage_h: 3,
        ..header()
    };
    assert_eq!(FrameHeader::decode(&header.encode()), Ok(header));
}

#[test]
fn empty_frame_means_no_scanout() {
    let blank = FrameHeader {
        width: 0,
        height: 0,
        stride: 0,
        format: 0,
        sequence: 3,
        payload_len: 0,
        flags: 0,
        scanout: 0,
        damage_x: 0,
        damage_y: 0,
        damage_w: 0,
        damage_h: 0,
        scale_x100: 100,
    };
    assert_eq!(FrameHeader::decode(&blank.encode()), Ok(blank));
}

#[test]
fn frame_header_rejects_bad_magic_version_kind_and_geometry() {
    let mut bytes = header().encode();
    bytes[0] = b'X';
    assert!(matches!(
        FrameHeader::decode(&bytes),
        Err(WireError::BadMagic { .. })
    ));

    let mut bytes = header().encode();
    bytes[4] = 1;
    let err = FrameHeader::decode(&bytes).expect_err("version 1");
    assert_eq!(err, WireError::Version { found: 1 });
    assert!(err.to_string().contains("build-viewer.sh"), "{err}");

    let mut bytes = header().encode();
    bytes[6] = 9;
    assert_eq!(
        FrameHeader::decode(&bytes),
        Err(WireError::Kind { kind: 9 })
    );

    for bad in [
        FrameHeader {
            stride: 2559,
            ..header()
        },
        FrameHeader {
            payload_len: 1,
            ..header()
        },
        FrameHeader {
            width: MAX_DIMENSION + 1,
            stride: (MAX_DIMENSION + 1) * 4,
            height: 1,
            payload_len: (MAX_DIMENSION + 1) * 4,
            ..header()
        },
    ] {
        assert!(
            matches!(
                FrameHeader::decode(&bad.encode()),
                Err(WireError::Geometry { .. })
            ),
            "{bad:?}"
        );
    }

    assert!(matches!(
        FrameHeader::decode(&header().encode()[..63]),
        Err(WireError::Short { need: 64, .. })
    ));
}

#[test]
fn viewer_messages_round_trip() {
    for message in [
        ViewerMessage::Hello { version: VERSION },
        ViewerMessage::Presented { sequence: u64::MAX },
        ViewerMessage::SetSize {
            width: 1920,
            height: 1080,
        },
        ViewerMessage::SetSizeEx {
            scanout: 2,
            width: 800,
            height: 600,
        },
    ] {
        let bytes = message.encode();
        assert_eq!(bytes.len(), MESSAGE_LEN);
        assert_eq!(ViewerMessage::decode(&bytes), Ok(message));
    }
    let hello = ViewerMessage::Hello { version: VERSION }.encode();
    assert_eq!(&hello[6..8], &KIND_HELLO.to_le_bytes());
    assert_eq!(&hello[8..16], &4u64.to_le_bytes());
}

#[test]
fn input_messages_round_trip_with_documented_layout() {
    let cases = [
        (
            ViewerInput::Key {
                mac_keycode: 0x7e,
                pressed: true,
            },
            KIND_KEY,
            0x1_007e_u64,
        ),
        (
            ViewerInput::Key {
                mac_keycode: 0,
                pressed: false,
            },
            KIND_KEY,
            0,
        ),
        (
            ViewerInput::Pointer {
                x: POINTER_MAX,
                y: 5,
            },
            KIND_POINTER,
            0x5_0000_7fff,
        ),
        (
            ViewerInput::Button {
                button: PointerButton::Middle,
                pressed: true,
            },
            KIND_BUTTON,
            0x102,
        ),
        (
            ViewerInput::Scroll { dx: 1, dy: -1 },
            KIND_SCROLL,
            0xffff_ffff_0000_0001,
        ),
        (
            ViewerInput::RelMotion { dx: -2, dy: 3 },
            KIND_REL,
            0x0000_0003_ffff_fffe,
        ),
        (ViewerInput::Capture { on: true }, KIND_CAPTURE, 1),
        (
            ViewerInput::GamepadButton {
                code: 0x130,
                pressed: true,
            },
            KIND_GP_BTN,
            0x1_0130,
        ),
        (
            ViewerInput::GamepadAxis {
                code: 0,
                value: -32768,
            },
            KIND_GP_AXIS,
            u64::from((-32768i32) as u32) << 16,
        ),
    ];
    for (input, kind, value) in cases {
        let bytes = ViewerMessage::Input(input).encode();
        assert_eq!(&bytes[6..8], &kind.to_le_bytes(), "{input:?}");
        assert_eq!(&bytes[8..16], &value.to_le_bytes(), "{input:?}");
        assert_eq!(
            ViewerMessage::decode(&bytes),
            Ok(ViewerMessage::Input(input))
        );
    }
}

#[test]
fn input_messages_reject_out_of_range_values() {
    let raw = |kind: u16, value: u64| {
        let mut bytes = ViewerMessage::Presented { sequence: value }.encode();
        bytes[6..8].copy_from_slice(&kind.to_le_bytes());
        ViewerMessage::decode(&bytes)
    };
    for (kind, value) in [
        (KIND_POINTER, u64::from(POINTER_MAX) + 1),
        (KIND_POINTER, u64::from(POINTER_MAX + 1) << 32),
        (KIND_BUTTON, 3),
        (KIND_BUTTON, 0x200),
        (KIND_KEY, 0x2_0000),
    ] {
        assert_eq!(raw(kind, value), Err(WireError::Value { kind, value }));
    }
    assert_eq!(raw(0x0299, 0), Err(WireError::Kind { kind: 0x0299 }));
}

#[test]
fn viewer_message_rejects_unknown_kind_and_short_input() {
    let mut bytes = ViewerMessage::Presented { sequence: 1 }.encode();
    bytes[6..8].copy_from_slice(&KIND_FRAME.to_le_bytes());
    assert_eq!(
        ViewerMessage::decode(&bytes),
        Err(WireError::Kind { kind: KIND_FRAME })
    );
    assert!(matches!(
        ViewerMessage::decode(&bytes[..8]),
        Err(WireError::Short { need: 16, .. })
    ));
}
