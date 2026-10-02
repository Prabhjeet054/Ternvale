//! Encode/decode edge cases: exact bytes, every truncation point, and the
//! frame size limit from both sides.

use std::collections::VecDeque;
use std::io::{Cursor, ErrorKind, Read};

use super::*;

/// Each message with the JSON payload it must encode to.
fn golden() -> Vec<(Message, &'static str)> {
    vec![
        (
            Message::Hello {
                version: 1,
                os: "Linux 6.6 aarch64".to_string(),
                agent: "ternvale-agent 0.1.0".to_string(),
            },
            r#"{"type":"hello","version":1,"os":"Linux 6.6 aarch64","agent":"ternvale-agent 0.1.0"}"#,
        ),
        (
            Message::Welcome { version: 1 },
            r#"{"type":"welcome","version":1}"#,
        ),
        (
            Message::Reject {
                reason: "old".to_string(),
                min_version: 1,
                max_version: 2,
            },
            r#"{"type":"reject","reason":"old","min_version":1,"max_version":2}"#,
        ),
        (Message::Ping { seq: 0 }, r#"{"type":"ping","seq":0}"#),
        (
            Message::Pong { seq: u64::MAX },
            r#"{"type":"pong","seq":18446744073709551615}"#,
        ),
        (
            Message::SetResolution {
                width: 1280,
                height: 800,
            },
            r#"{"type":"set_resolution","width":1280,"height":800}"#,
        ),
        (
            Message::ClipboardSet {
                text: ClipboardText("a\"b\n".to_string()),
            },
            r#"{"type":"clipboard_set","text":"a\"b\n"}"#,
        ),
        (Message::Shutdown, r#"{"type":"shutdown"}"#),
    ]
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(payload);
    bytes
}

fn is_eof(error: &FrameError) -> bool {
    matches!(error, FrameError::Io(e) if e.kind() == ErrorKind::UnexpectedEof)
}

/// Returns each chunk in turn with a `WouldBlock` between chunks, then EOF.
struct Chunks(VecDeque<Option<Vec<u8>>>);

impl Chunks {
    fn new(chunks: &[&[u8]]) -> Self {
        let mut steps = VecDeque::new();
        for (i, chunk) in chunks.iter().enumerate() {
            if i > 0 {
                steps.push_back(None);
            }
            steps.push_back(Some(chunk.to_vec()));
        }
        Self(steps)
    }
}

impl Read for Chunks {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.0.pop_front() {
            None => Ok(0),
            Some(None) => Err(ErrorKind::WouldBlock.into()),
            Some(Some(mut chunk)) => {
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    chunk.drain(..n);
                    self.0.push_front(Some(chunk));
                }
                Ok(n)
            }
        }
    }
}

/// Hands out a 4-byte prefix, then fails any further read, so a test can
/// prove the payload was never read.
struct PrefixOnly {
    prefix: [u8; 4],
    reads: usize,
}

impl Read for PrefixOnly {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reads += 1;
        if self.reads > 1 {
            return Err(std::io::Error::other("payload was read"));
        }
        buf[..4].copy_from_slice(&self.prefix);
        Ok(4)
    }
}

#[test]
fn every_message_encodes_to_its_documented_bytes_and_back() {
    for (message, json) in golden() {
        let bytes = encode(&message).expect("encode");
        assert_eq!(bytes, frame(json.as_bytes()), "{}", message.kind());
        let mut wire = Vec::new();
        write_message(&mut wire, &message, "test").expect("write");
        assert_eq!(wire, bytes, "{}", message.kind());
        let decoded = read_message(&mut Cursor::new(&bytes), "test").expect("decode");
        assert_eq!(decoded, message);
    }
}

#[test]
fn every_truncation_point_is_an_error_and_never_a_message() {
    for (message, _) in golden() {
        let bytes = encode(&message).expect("encode");
        for cut in 0..bytes.len() {
            let what = format!("{} cut at {cut}/{}", message.kind(), bytes.len());
            let short = &bytes[..cut];
            let error = read_message(&mut Cursor::new(short), "test").expect_err(&what);
            let error_by_reader = FrameReader::new("test")
                .read(&mut Cursor::new(short))
                .expect_err(&what);
            if cut == 0 {
                assert!(matches!(error, FrameError::Closed), "{what}: {error}");
                assert!(matches!(error_by_reader, FrameError::Closed), "{what}");
            } else {
                assert!(is_eof(&error), "{what}: {error}");
                assert!(is_eof(&error_by_reader), "{what}: {error_by_reader}");
                assert!(!error.is_skippable() && !error.is_timeout(), "{what}");
            }
        }
    }
}

#[test]
fn frame_reader_completes_a_frame_split_at_any_point() {
    for (message, _) in golden() {
        let bytes = encode(&message).expect("encode");
        for cut in 1..bytes.len() {
            let mut source = Chunks::new(&[&bytes[..cut], &bytes[cut..]]);
            let mut reader = FrameReader::new("test");
            let first = reader.read(&mut source).expect_err("first half");
            assert!(first.is_timeout(), "{} cut {cut}: {first}", message.kind());
            assert_eq!(reader.buffered(), cut);
            assert_eq!(reader.read(&mut source).expect("second half"), message);
            assert_eq!(reader.buffered(), 0);
            assert!(matches!(reader.read(&mut source), Err(FrameError::Closed)));
        }
    }
}

fn clipboard_with_payload_len(len: usize) -> Message {
    let overhead = encode(&Message::ClipboardSet {
        text: ClipboardText(String::new()),
    })
    .expect("encode")
    .len()
        - 4;
    Message::ClipboardSet {
        text: ClipboardText("x".repeat(len - overhead)),
    }
}

#[test]
fn a_payload_of_exactly_max_frame_passes_both_ways() {
    let message = clipboard_with_payload_len(MAX_FRAME);
    let bytes = encode(&message).expect("encode at the limit");
    assert_eq!(bytes.len(), 4 + MAX_FRAME);
    assert_eq!(&bytes[..4], &(MAX_FRAME as u32).to_be_bytes());
    assert_eq!(
        read_message(&mut Cursor::new(&bytes), "test").expect("read_message"),
        message
    );
    let mut reader = FrameReader::new("test");
    assert_eq!(
        reader.read(&mut Cursor::new(&bytes)).expect("FrameReader"),
        message
    );
    assert_eq!(reader.buffered(), 0);
}

#[test]
fn one_byte_over_max_frame_is_refused_on_send_and_writes_nothing() {
    let message = clipboard_with_payload_len(MAX_FRAME + 1);
    let error = encode(&message).expect_err("over the limit");
    assert!(
        matches!(error, FrameError::Encode { kind: "clipboard_set", ref reason } if reason.contains(&(MAX_FRAME + 1).to_string())),
        "{error}"
    );
    let mut wire = Vec::new();
    write_message(&mut wire, &message, "test").expect_err("over the limit");
    assert!(wire.is_empty());
}

#[test]
fn oversized_length_prefixes_are_rejected_without_reading_the_payload() {
    for len in [MAX_FRAME as u32 + 1, 1 << 31, u32::MAX] {
        let mut source = PrefixOnly {
            prefix: len.to_be_bytes(),
            reads: 0,
        };
        let error = read_message(&mut source, "test").expect_err("oversized");
        assert!(
            matches!(error, FrameError::BadLength { len: got, max: MAX_FRAME } if got == len),
            "{len}: {error}"
        );
        assert_eq!(source.reads, 1, "{len}: the payload must not be read");
        assert!(!error.is_skippable());
    }
}

#[test]
fn oversized_frames_are_rejected_even_when_the_bytes_follow() {
    let payload = vec![b'x'; MAX_FRAME + 1];
    let bytes = frame(&payload);
    let error = read_message(&mut Cursor::new(&bytes), "test").expect_err("read_message");
    assert!(matches!(error, FrameError::BadLength { .. }), "{error}");
    let error = FrameReader::new("test")
        .read(&mut Cursor::new(&bytes))
        .expect_err("FrameReader");
    assert!(matches!(error, FrameError::BadLength { .. }), "{error}");
}

#[test]
fn bad_payloads_inside_a_valid_frame_are_malformed() {
    let cases: [&[u8]; 10] = [
        &[0xff, 0xfe, 0x00],
        b"null",
        b"[]",
        br#"{"type":5}"#,
        br#"{"type":"ping","seq":"1"}"#,
        br#"{"type":"ping","seq":-1}"#,
        br#"{"type":"ping","seq":1.5}"#,
        br#"{"type":"ping","seq":1} trailing"#,
        br#"{"type":"set_resolution","width":4294967296,"height":1}"#,
        br#"{"type":"clipboard_set","text":"\ud800"}"#,
    ];
    for payload in cases {
        let shown = String::from_utf8_lossy(payload);
        let mut wire = frame(payload);
        wire.extend(encode(&Message::Ping { seq: 2 }).expect("encode"));
        let mut cursor = Cursor::new(wire);
        let error = read_message(&mut cursor, "test").expect_err(&shown);
        assert!(
            matches!(error, FrameError::Malformed { .. }),
            "{shown}: {error}"
        );
        assert!(error.is_skippable());
        assert_eq!(
            read_message(&mut cursor, "test").expect("stream stays in sync"),
            Message::Ping { seq: 2 }
        );
    }
}
