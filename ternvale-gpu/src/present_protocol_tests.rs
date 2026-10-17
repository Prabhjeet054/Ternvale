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
    assert_eq!(&bytes[4..8], &[1, 0, 1, 0], "version 1, kind FRAME");
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
    bytes[4] = 2;
    let err = FrameHeader::decode(&bytes).expect_err("version 2");
    assert_eq!(err, WireError::Version { found: 2 });
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
    assert_eq!(&hello[8..16], &1u64.to_le_bytes());
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
