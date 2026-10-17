use super::*;

fn header() -> FrameHeader {
    FrameHeader {
        width: 640,
        height: 480,
        stride: 2560,
        format: 2,
        sequence: 0x1122_3344_5566_7788,
        payload_len: 2560 * 480,
    }
}

#[test]
fn frame_header_round_trips_little_endian() {
    let bytes = header().encode();
    assert_eq!(&bytes[0..4], b"TVDP");
    assert_eq!(&bytes[4..8], &[2, 0, 1, 0], "version 2, kind FRAME");
    assert_eq!(&bytes[8..12], &640u32.to_le_bytes());
    assert_eq!(&bytes[24..32], &0x1122_3344_5566_7788u64.to_le_bytes());
    assert_eq!(&bytes[36..40], &[0; 4], "reserved");
    assert_eq!(FrameHeader::decode(&bytes), Ok(header()));
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
        FrameHeader::decode(&header().encode()[..39]),
        Err(WireError::Short { need: 40, .. })
    ));
}

#[test]
fn viewer_messages_round_trip() {
    for message in [
        ViewerMessage::Hello { version: VERSION },
        ViewerMessage::Presented { sequence: u64::MAX },
    ] {
        let bytes = message.encode();
        assert_eq!(bytes.len(), MESSAGE_LEN);
        assert_eq!(ViewerMessage::decode(&bytes), Ok(message));
    }
    let hello = ViewerMessage::Hello { version: VERSION }.encode();
    assert_eq!(&hello[6..8], &KIND_HELLO.to_le_bytes());
    assert_eq!(&hello[8..16], &2u64.to_le_bytes());
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
