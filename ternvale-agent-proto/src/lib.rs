//! Wire protocol between the Ternvale host and the in-guest `ternvale-agent`.
//!
//! The agent connects to host CID 2, port [`AGENT_PORT`], over virtio-vsock.
//! Each frame is a 4-byte big-endian payload length followed by one JSON
//! object tagged by `type` (see [`Message`]). Frames are at most
//! [`MAX_FRAME`] bytes.
//!
//! Versioning: the agent opens with `Hello { version }`. The host answers
//! `Welcome { version }` with the version both sides will speak
//! ([`negotiate`]), or `Reject` and closes. Receivers skip message types they
//! do not know and ignore unknown fields, so a minor addition does not need
//! a version bump; changing or removing a message does.

mod frame;
mod message;
mod reader;

pub use frame::{encode, read_message, write_message, FrameError};
pub use message::{ClipboardText, Message};
pub use reader::FrameReader;

/// Version this build speaks (and the newest it accepts).
pub const PROTOCOL_VERSION: u32 = 1;
/// Oldest version this build still accepts.
pub const MIN_PROTOCOL_VERSION: u32 = 1;
/// vsock port the host agent server listens on.
pub const AGENT_PORT: u32 = 5000;
/// Largest frame payload, in bytes.
pub const MAX_FRAME: usize = 256 * 1024;
/// Longest `os` / `agent` string in `Hello`.
pub const MAX_NAME: usize = 256;
/// Largest `SetResolution` dimension.
pub const MAX_DIMENSION: u32 = 16_384;

/// The peer's protocol version is outside what this build supports.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("protocol version {peer} is not supported (this side speaks {min}..={max})")]
pub struct VersionError {
    /// Version the peer offered.
    pub peer: u32,
    /// [`MIN_PROTOCOL_VERSION`].
    pub min: u32,
    /// [`PROTOCOL_VERSION`].
    pub max: u32,
}

/// The version both sides speak when the peer offers `peer`: the lower of
/// the two, as long as it is not below [`MIN_PROTOCOL_VERSION`].
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(peer))]
pub fn negotiate(peer: u32) -> Result<u32, VersionError> {
    if peer < MIN_PROTOCOL_VERSION {
        tracing::warn!(target: "ternvale::agent", peer, min = MIN_PROTOCOL_VERSION, max = PROTOCOL_VERSION, "protocol version rejected");
        return Err(VersionError {
            peer,
            min: MIN_PROTOCOL_VERSION,
            max: PROTOCOL_VERSION,
        });
    }
    let version = peer.min(PROTOCOL_VERSION);
    tracing::debug!(target: "ternvale::agent", peer, version, "protocol version negotiated");
    Ok(version)
}

#[cfg(test)]
mod codec_tests;
#[cfg(test)]
mod tests;
