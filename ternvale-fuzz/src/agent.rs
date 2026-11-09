//! Agent protocol decoder (`FrameReader`).

use std::io::Cursor;

use ternvale_agent_proto::FrameReader;

use crate::quiet;

/// Feed a length-prefixed JSON stream into the host decoder.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let mut reader = FrameReader::new("fuzz");
    let mut cursor = Cursor::new(data);
    while reader.read(&mut cursor).is_ok() {
        if cursor.position() as usize >= data.len() {
            break;
        }
    }
}
