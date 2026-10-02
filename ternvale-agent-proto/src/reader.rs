//! Buffered frame reader for sockets polled with a read timeout.

use std::io::{ErrorKind, Read};

use crate::frame::{check_length, decode};
use crate::{FrameError, Message};

/// Reads frames from a stream, keeping partial data across errors, so a
/// read timeout (`WouldBlock` / `TimedOut`) in the middle of a frame does
/// not lose sync. Use one `FrameReader` per connection.
#[derive(Debug)]
pub struct FrameReader {
    peer: &'static str,
    buffer: Vec<u8>,
}

impl FrameReader {
    /// A reader for a stream from `peer` (used in logs).
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(peer))]
    pub fn new(peer: &'static str) -> Self {
        Self {
            peer,
            buffer: Vec::new(),
        }
    }

    /// Bytes received but not yet returned as a message.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// The next message. Errors from `reader` are returned as-is with the
    /// partial frame kept; call again after a timeout.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(peer = self.peer))]
    pub fn read<R: Read>(&mut self, reader: &mut R) -> Result<Message, FrameError> {
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(result) = self.take_frame() {
                return result;
            }
            match reader.read(&mut chunk) {
                Ok(0) if self.buffer.is_empty() => return Err(FrameError::Closed),
                Ok(0) => {
                    tracing::warn!(target: "ternvale::agent", peer = self.peer, buffered = self.buffer.len(), "agent stream closed mid-frame");
                    return Err(std::io::Error::from(ErrorKind::UnexpectedEof).into());
                }
                Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn take_frame(&mut self) -> Option<Result<Message, FrameError>> {
        let prefix: [u8; 4] = self.buffer.get(..4)?.try_into().ok()?;
        let len = match check_length(u32::from_be_bytes(prefix), self.peer) {
            Ok(len) => len,
            Err(error) => return Some(Err(error)),
        };
        if self.buffer.len() < 4 + len {
            return None;
        }
        let frame: Vec<u8> = self.buffer.drain(..4 + len).collect();
        Some(decode(&frame[4..], self.peer))
    }
}
