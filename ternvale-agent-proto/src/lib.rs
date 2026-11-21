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

mod app;
mod clip;
mod drag;
mod frame;
mod hex;
mod message;
mod print;
mod reader;
mod window;

pub use app::{
    validate_app_launch, validate_apps, validate_host_open, AppSource, GuestApp, MAX_APPS,
    MAX_APP_EXE, MAX_APP_EXT, MAX_APP_EXTS, MAX_APP_ICON, MAX_APP_ID, MAX_APP_NAME,
};
pub use clip::{
    check_inline_len, is_clip_format, is_echo_seq, negotiate_format, validate_clipboard_offer,
    validate_clipboard_payload, validate_clipboard_take, validate_reason, CLIP_FORMATS,
    MAX_CLIP_FILES, MAX_CLIP_FORMATS, MAX_CLIP_INLINE,
};
pub use drag::{
    validate_drag_accept, validate_drag_cancel, validate_drag_id, validate_drag_offer,
    validate_drag_progress, MAX_DRAG_FILES, MAX_DRAG_ID,
};
pub use frame::{encode, read_message, write_message, FrameError};
pub use hex::{decode as decode_hex, encode as encode_hex, HexBytes};
pub use message::{is_guest_file_path, ClipboardText, ExecStream, Message};
pub use print::{
    slug_printer_id, valid_printer_id, validate_printers, GuestPrinter, IPP_LOCAL_PORT, IPP_PORT,
    MAX_PRINTERS, MAX_PRINTER_ID, MAX_PRINTER_NAME, PDF_PRINTER_ID, PDF_PRINTER_NAME,
};
pub use reader::FrameReader;
pub use window::{
    validate_window_frame, validate_window_input, validate_window_op, validate_windows,
    GuestWindow, WindowKind, WindowState, MAX_WINDOWS, MAX_WINDOW_DIMENSION,
    MAX_WINDOW_FRAME_BYTES, MAX_WINDOW_ICON, MAX_WINDOW_TITLE,
};

/// Version this build speaks (and the newest it accepts).
pub const PROTOCOL_VERSION: u32 = 3;
/// Oldest version this build still accepts.
pub const MIN_PROTOCOL_VERSION: u32 = 1;
/// vsock port the host agent server listens on.
pub const AGENT_PORT: u32 = 5000;
/// virtio-console multiport name (`vioserial` / `virtio_console`).
///
/// Linux: `/dev/virtio-ports/org.ternvale.agent`.
/// Windows: `\\.\Global\org.ternvale.agent`.
pub const AGENT_SERIAL_NAME: &str = "org.ternvale.agent";
/// Largest frame payload, in bytes.
pub const MAX_FRAME: usize = 256 * 1024;
/// Longest `os` / `agent` string in `Hello`.
pub const MAX_NAME: usize = 256;
/// Largest `SetResolution` dimension.
pub const MAX_DIMENSION: u32 = 16_384;
/// Longest virtio-fs mount tag in `ShareMount`.
pub const MAX_TAG_LEN: usize = 36;
/// Longest guest mount path in `ShareMount`.
pub const MAX_MOUNT_PATH: usize = 256;
/// Longest guest file path in copy messages.
pub const MAX_COPY_PATH: usize = 512;
/// Max argv entries in `Exec`.
pub const MAX_EXEC_ARGV: usize = 32;
/// Longest single `Exec` argument.
pub const MAX_EXEC_ARG: usize = 4096;
/// Largest binary chunk in `ExecOutput` / `CopyPut` / `CopyGetOk` (before hex).
pub const MAX_BIN_CHUNK: usize = 24 * 1024;
/// Largest `DriverLogsOk.text` (JSON must still fit [`MAX_FRAME`]).
pub const MAX_DRIVER_LOG_BYTES: usize = 48 * 1024;

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
