//! Framing: `u32` big-endian length, then that many bytes of JSON.

use std::io::{ErrorKind, Read, Write};

use crate::{Message, MAX_FRAME};

/// Why a frame could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The peer closed the stream cleanly between frames.
    #[error("peer closed the connection")]
    Closed,
    /// Socket error, including a timeout or EOF in the middle of a frame.
    #[error("agent socket i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The length prefix is zero or above [`MAX_FRAME`]. The stream is out of
    /// sync and must be closed.
    #[error("frame length {len} outside 1..={max}")]
    BadLength {
        /// Length from the prefix.
        len: u32,
        /// [`MAX_FRAME`].
        max: usize,
    },
    /// A well-formed frame with a `type` this build does not know. The frame
    /// was consumed; skipping it keeps newer peers compatible.
    #[error("unknown message type {kind:?}")]
    UnknownType {
        /// The `type` tag.
        kind: String,
    },
    /// The payload is not a valid message, or a field is out of range. The
    /// frame was consumed.
    #[error("malformed message: {reason}")]
    Malformed {
        /// Parser or validation error.
        reason: String,
    },
    /// The message could not be encoded (or is larger than [`MAX_FRAME`]).
    #[error("cannot encode {kind} message: {reason}")]
    Encode {
        /// Message type.
        kind: &'static str,
        /// Why.
        reason: String,
    },
}

impl FrameError {
    /// The frame was consumed and the stream is still in sync: log and keep reading.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn is_skippable(&self) -> bool {
        matches!(self, Self::UnknownType { .. } | Self::Malformed { .. })
    }

    /// A read timeout (`SO_RCVTIMEO`) expired.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Io(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut))
    }
}

/// Encode and send `message`, logging it at DEBUG. `peer` names the other
/// side in the log (`host` or `agent`).
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(peer, kind = message.kind()))]
pub fn write_message<W: Write>(
    writer: &mut W,
    message: &Message,
    peer: &str,
) -> Result<(), FrameError> {
    let frame = encode(message)?;
    writer.write_all(&frame)?;
    writer.flush()?;
    tracing::debug!(target: "ternvale::agent", direction = "send", peer, kind = message.kind(), bytes = frame.len() - 4, msg = ?message, "agent message");
    Ok(())
}

/// Encode `message` as one complete frame: the big-endian length, then the
/// JSON payload. Fails with [`FrameError::Encode`] when the payload is over
/// [`MAX_FRAME`].
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(kind = message.kind()))]
pub fn encode(message: &Message) -> Result<Vec<u8>, FrameError> {
    let payload = serde_json::to_vec(message).map_err(|error| FrameError::Encode {
        kind: message.kind(),
        reason: error.to_string(),
    })?;
    if payload.len() > MAX_FRAME {
        tracing::warn!(target: "ternvale::agent", kind = message.kind(), bytes = payload.len(), max = MAX_FRAME, "message too large to send");
        return Err(FrameError::Encode {
            kind: message.kind(),
            reason: format!("{} bytes exceeds {MAX_FRAME}", payload.len()),
        });
    }
    let len = u32::try_from(payload.len()).map_err(|_| FrameError::Encode {
        kind: message.kind(),
        reason: "length does not fit u32".to_string(),
    })?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Read exactly one frame and decode it, logging it at DEBUG. See
/// [`FrameError`] for which errors leave the stream usable. On a socket with
/// a read timeout use [`crate::FrameReader`], which keeps a partial frame.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(peer))]
pub fn read_message<R: Read>(reader: &mut R, peer: &str) -> Result<Message, FrameError> {
    let mut prefix = [0u8; 4];
    let mut got = 0;
    while got < prefix.len() {
        match reader.read(&mut prefix[got..]) {
            Ok(0) if got == 0 => return Err(FrameError::Closed),
            Ok(0) => return Err(std::io::Error::from(ErrorKind::UnexpectedEof).into()),
            Ok(n) => got += n,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    let len = check_length(u32::from_be_bytes(prefix), peer)?;
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    decode(&payload, peer)
}

/// Check a length prefix against [`MAX_FRAME`].
pub(crate) fn check_length(len: u32, peer: &str) -> Result<usize, FrameError> {
    if len == 0 || len as usize > MAX_FRAME {
        tracing::warn!(target: "ternvale::agent", peer, len, max = MAX_FRAME, "agent frame length rejected");
        return Err(FrameError::BadLength {
            len,
            max: MAX_FRAME,
        });
    }
    Ok(len as usize)
}

/// Decode, validate and log one received payload.
pub(crate) fn decode(payload: &[u8], peer: &str) -> Result<Message, FrameError> {
    let message = parse(payload, peer)?;
    tracing::debug!(target: "ternvale::agent", direction = "recv", peer, kind = message.kind(), bytes = payload.len(), msg = ?message, "agent message");
    Ok(message)
}

fn parse(payload: &[u8], peer: &str) -> Result<Message, FrameError> {
    let error = match serde_json::from_slice::<Message>(payload) {
        Ok(message) => {
            return match message.validate() {
                Ok(()) => Ok(message),
                Err(reason) => {
                    tracing::warn!(target: "ternvale::agent", peer, kind = message.kind(), reason = %reason, "agent message out of range");
                    Err(FrameError::Malformed { reason })
                }
            }
        }
        Err(error) => error,
    };
    let kind = serde_json::from_slice::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| value.get("type")?.as_str().map(str::to_string));
    match kind {
        Some(kind) if !KNOWN.contains(&kind.as_str()) => {
            tracing::warn!(target: "ternvale::agent", peer, kind = %kind, bytes = payload.len(), "unknown agent message type skipped");
            Err(FrameError::UnknownType { kind })
        }
        _ => {
            tracing::warn!(target: "ternvale::agent", peer, error = %error, bytes = payload.len(), "malformed agent message");
            Err(FrameError::Malformed {
                reason: error.to_string(),
            })
        }
    }
}

const KNOWN: [&str; 8] = [
    "hello",
    "welcome",
    "reject",
    "ping",
    "pong",
    "set_resolution",
    "clipboard_set",
    "shutdown",
];
