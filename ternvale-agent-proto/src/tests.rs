use std::io::Cursor;

use super::*;

fn all_messages() -> Vec<Message> {
    vec![
        Message::Hello {
            version: PROTOCOL_VERSION,
            os: "Linux 6.6.58-0-virt aarch64".to_string(),
            agent: "ternvale-agent 0.1.0".to_string(),
        },
        Message::Welcome { version: 1 },
        Message::Reject {
            reason: "too old".to_string(),
            min_version: 1,
            max_version: 1,
        },
        Message::Ping { seq: 7 },
        Message::Pong { seq: u64::MAX },
        Message::SetResolution {
            width: 1920,
            height: 1080,
        },
        Message::ClipboardSet {
            text: ClipboardText("héllo\nworld".to_string()),
        },
        Message::Shutdown,
    ]
}

fn frame(json: &str) -> Vec<u8> {
    let mut bytes = (json.len() as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(json.as_bytes());
    bytes
}

#[test]
fn every_message_round_trips_through_one_stream() {
    let mut wire = Vec::new();
    for message in all_messages() {
        write_message(&mut wire, &message, "test").expect("encode");
    }
    let mut reader = Cursor::new(wire);
    for expected in all_messages() {
        assert_eq!(read_message(&mut reader, "test").expect("decode"), expected);
    }
    assert!(matches!(
        read_message(&mut reader, "test"),
        Err(FrameError::Closed)
    ));
}

#[test]
fn wire_format_is_big_endian_length_then_tagged_json() {
    let mut wire = Vec::new();
    write_message(&mut wire, &Message::Ping { seq: 3 }, "test").expect("encode");
    assert_eq!(wire, frame(r#"{"type":"ping","seq":3}"#));
    let mut wire = Vec::new();
    write_message(&mut wire, &Message::Shutdown, "test").expect("encode");
    assert_eq!(wire, frame(r#"{"type":"shutdown"}"#));
}

#[test]
fn hello_without_agent_field_and_with_extra_fields_decodes() {
    let bytes = frame(r#"{"type":"hello","version":1,"os":"Linux","future":true}"#);
    let message = read_message(&mut Cursor::new(bytes), "test").expect("decode");
    assert_eq!(
        message,
        Message::Hello {
            version: 1,
            os: "Linux".to_string(),
            agent: String::new()
        }
    );
}

#[test]
fn unknown_type_is_skippable_and_the_stream_stays_in_sync() {
    let mut bytes = frame(r#"{"type":"set_wallpaper","url":"x"}"#);
    bytes.extend(frame(r#"{"type":"ping","seq":1}"#));
    let mut reader = Cursor::new(bytes);
    let error = read_message(&mut reader, "test").expect_err("unknown");
    assert!(error.is_skippable());
    assert!(matches!(error, FrameError::UnknownType { ref kind } if kind == "set_wallpaper"));
    assert_eq!(
        read_message(&mut reader, "test").expect("next frame"),
        Message::Ping { seq: 1 }
    );
}

#[test]
fn malformed_and_out_of_range_payloads_are_skippable() {
    for json in [
        r#"{"type":"ping"}"#,
        r#"not json"#,
        r#"{"seq":1}"#,
        r#"{"type":"set_resolution","width":0,"height":10}"#,
        r#"{"type":"set_resolution","width":10,"height":99999}"#,
    ] {
        let error = read_message(&mut Cursor::new(frame(json)), "test").expect_err(json);
        assert!(
            matches!(error, FrameError::Malformed { .. }),
            "{json}: {error}"
        );
        assert!(error.is_skippable());
    }
    let long = "x".repeat(MAX_NAME + 1);
    let json = format!(r#"{{"type":"hello","version":1,"os":"{long}"}}"#);
    let error = read_message(&mut Cursor::new(frame(&json)), "test").expect_err("long os");
    assert!(matches!(error, FrameError::Malformed { .. }));
}

#[test]
fn bad_lengths_are_rejected_before_allocating() {
    for len in [0u32, MAX_FRAME as u32 + 1, u32::MAX] {
        let error =
            read_message(&mut Cursor::new(len.to_be_bytes().to_vec()), "test").expect_err("length");
        assert!(matches!(error, FrameError::BadLength { len: got, .. } if got == len));
        assert!(!error.is_skippable());
    }
}

#[test]
fn truncated_frames_are_io_errors_not_clean_closes() {
    let error = read_message(&mut Cursor::new(vec![0, 0]), "test").expect_err("short prefix");
    assert!(
        matches!(error, FrameError::Io(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof)
    );
    let mut bytes = frame(r#"{"type":"ping","seq":1}"#);
    bytes.truncate(10);
    let error = read_message(&mut Cursor::new(bytes), "test").expect_err("short payload");
    assert!(matches!(error, FrameError::Io(_)));
    assert!(!error.is_timeout());
}

#[test]
fn oversized_messages_are_refused_on_send() {
    let message = Message::ClipboardSet {
        text: ClipboardText("x".repeat(MAX_FRAME)),
    };
    let mut wire = Vec::new();
    let error = write_message(&mut wire, &message, "test").expect_err("too big");
    assert!(matches!(
        error,
        FrameError::Encode {
            kind: "clipboard_set",
            ..
        }
    ));
    assert!(wire.is_empty());
}

#[test]
fn clipboard_debug_output_hides_the_text() {
    let message = Message::ClipboardSet {
        text: ClipboardText("secret".to_string()),
    };
    let shown = format!("{message:?}");
    assert!(shown.contains("<6 bytes>"), "{shown}");
    assert!(!shown.contains("secret"));
}

#[test]
fn negotiation_picks_the_lower_version_and_rejects_old_peers() {
    assert_eq!(negotiate(PROTOCOL_VERSION), Ok(PROTOCOL_VERSION));
    assert_eq!(negotiate(PROTOCOL_VERSION + 5), Ok(PROTOCOL_VERSION));
    let error = negotiate(MIN_PROTOCOL_VERSION - 1).expect_err("too old");
    assert_eq!(error.peer, MIN_PROTOCOL_VERSION - 1);
    assert_eq!(
        error.to_string(),
        "protocol version 0 is not supported (this side speaks 1..=1)"
    );
}

/// Hands out one byte per read, with a timeout error before every byte.
struct Trickle {
    bytes: Vec<u8>,
    at: usize,
    timeout_next: bool,
}

impl std::io::Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.timeout_next = !self.timeout_next;
        if self.timeout_next {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        let Some(&byte) = self.bytes.get(self.at) else {
            return Ok(0);
        };
        buf[0] = byte;
        self.at += 1;
        Ok(1)
    }
}

#[test]
fn frame_reader_survives_timeouts_in_the_middle_of_frames() {
    let mut wire = Vec::new();
    for message in all_messages() {
        write_message(&mut wire, &message, "test").expect("encode");
    }
    let mut source = Trickle {
        bytes: wire,
        at: 0,
        timeout_next: false,
    };
    let mut reader = FrameReader::new("test");
    let mut got = Vec::new();
    let mut timeouts = 0;
    loop {
        match reader.read(&mut source) {
            Ok(message) => got.push(message),
            Err(error) if error.is_timeout() => timeouts += 1,
            Err(FrameError::Closed) => break,
            Err(error) => panic!("unexpected {error}"),
        }
    }
    assert_eq!(got, all_messages());
    assert!(timeouts > 100, "{timeouts}");
    assert_eq!(reader.buffered(), 0);
}

#[test]
fn frame_reader_skips_unknown_types_and_reports_mid_frame_eof() {
    let mut bytes = frame(r#"{"type":"future_thing"}"#);
    bytes.extend(frame(r#"{"type":"pong","seq":9}"#));
    bytes.extend_from_slice(&[0, 0, 0, 50, b'{']);
    let mut source = Cursor::new(bytes);
    let mut reader = FrameReader::new("test");
    assert!(reader
        .read(&mut source)
        .expect_err("unknown")
        .is_skippable());
    assert_eq!(
        reader.read(&mut source).expect("pong"),
        Message::Pong { seq: 9 }
    );
    let error = reader.read(&mut source).expect_err("eof");
    assert!(
        matches!(error, FrameError::Io(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof)
    );
}

#[test]
fn frame_reader_rejects_bad_lengths() {
    let mut source = Cursor::new((MAX_FRAME as u32 + 1).to_be_bytes().to_vec());
    let error = FrameReader::new("test")
        .read(&mut source)
        .expect_err("length");
    assert!(matches!(error, FrameError::BadLength { .. }));
}

#[test]
fn kind_matches_the_wire_tag() {
    for message in all_messages() {
        let json = serde_json::to_value(&message).expect("json");
        assert_eq!(json["type"], message.kind());
    }
}
