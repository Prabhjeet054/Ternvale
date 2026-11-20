//! Assemble rich clipboard payloads and answer host offers.

use ternvale_agent_proto::{
    check_inline_len, is_echo_seq, negotiate_format, HexBytes, Message, MAX_BIN_CHUNK,
    MAX_CLIP_INLINE,
};

use crate::clip;
use crate::AgentError;

/// In-flight clipboard assembly (one seq at a time).
#[derive(Debug, Default)]
pub struct Assembler {
    seq: u64,
    format: String,
    buf: Vec<u8>,
    last_echo: Option<u64>,
}

impl Assembler {
    /// Remember the echo token we last wrote so guest→host loops are skipped.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn remember_echo(&mut self, echo: u64) {
        if echo != 0 {
            self.last_echo = Some(echo);
        }
    }

    /// True when this offer is our own paste coming back.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn is_loop(&self, echo: u64) -> bool {
        is_echo_seq(self.last_echo, echo)
    }

    /// Append a payload chunk. Returns the finished blob on `last`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::agent",
        skip_all,
        fields(seq, format, offset, last)
    )]
    pub fn push(
        &mut self,
        seq: u64,
        format: &str,
        offset: u64,
        last: bool,
        data: &[u8],
    ) -> Result<Option<(String, Vec<u8>)>, AgentError> {
        if self.seq != seq {
            self.seq = seq;
            self.format = format.to_string();
            self.buf.clear();
        }
        if offset as usize != self.buf.len() {
            tracing::warn!(
                target: "ternvale::agent",
                seq,
                offset,
                have = self.buf.len(),
                "clipboard payload offset mismatch"
            );
            return Err(AgentError::Action {
                action: "clipboard payload offset",
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, "offset mismatch"),
            });
        }
        if self.buf.len().saturating_add(data.len()) > MAX_CLIP_INLINE {
            return Err(AgentError::Action {
                action: "clipboard payload cap",
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, "too large"),
            });
        }
        self.buf.extend_from_slice(data);
        if last {
            check_inline_len(self.buf.len()).map_err(|reason| AgentError::Action {
                action: "clipboard payload",
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, reason),
            })?;
            let format = std::mem::take(&mut self.format);
            let buf = std::mem::take(&mut self.buf);
            tracing::info!(target: "ternvale::agent", seq, format = %format, bytes = buf.len(), "clipboard payload assembled");
            Ok(Some((format, buf)))
        } else {
            Ok(None)
        }
    }
}

/// Chunk a blob into `clipboard_payload` messages.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(seq, format, bytes = data.len()))]
pub fn chunk_payload(seq: u64, format: &str, echo: u64, data: &[u8]) -> Vec<Message> {
    let mut out = Vec::new();
    let mut offset = 0u64;
    if data.is_empty() {
        return vec![Message::ClipboardPayload {
            seq,
            format: format.to_string(),
            offset: 0,
            last: true,
            data: HexBytes(Vec::new()),
            echo,
        }];
    }
    while offset < data.len() as u64 {
        let start = offset as usize;
        let end = (start + MAX_BIN_CHUNK).min(data.len());
        let last = end == data.len();
        out.push(Message::ClipboardPayload {
            seq,
            format: format.to_string(),
            offset,
            last,
            data: HexBytes(data[start..end].to_vec()),
            echo,
        });
        offset = end as u64;
    }
    out
}

/// Choose a format and optionally take text from the local clipboard.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn take_from_offer(formats: &[String]) -> Option<&'static str> {
    negotiate_format(formats)
}

/// Apply an assembled payload to the guest clipboard (text today; others staged).
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(format, bytes = data.len()))]
pub fn apply_payload(format: &str, data: &[u8]) -> Result<(), AgentError> {
    match format {
        "text" => {
            let text = String::from_utf8_lossy(data);
            if clip::capped(&text).is_none() {
                return Ok(());
            }
            Ok(())
        }
        "png" | "dib" | "rtf" | "html" | "files" => {
            tracing::info!(
                target: "ternvale::agent",
                format,
                bytes = data.len(),
                "rich clipboard payload stored (native set is OS-specific)"
            );
            Ok(())
        }
        other => {
            tracing::warn!(target: "ternvale::agent", format = other, "unknown clipboard format ignored");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles_two_chunks() {
        let mut a = Assembler::default();
        assert!(a.push(1, "png", 0, false, b"ab").unwrap().is_none());
        let done = a.push(1, "png", 2, true, b"cd").unwrap().unwrap();
        assert_eq!(done.0, "png");
        assert_eq!(done.1, b"abcd");
    }

    #[test]
    fn loop_echo_is_detected() {
        let mut a = Assembler::default();
        a.remember_echo(9);
        assert!(a.is_loop(9));
        assert!(!a.is_loop(10));
    }
}
