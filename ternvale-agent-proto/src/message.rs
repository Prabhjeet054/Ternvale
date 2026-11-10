//! Protocol messages.

use serde::{Deserialize, Serialize};

use crate::{
    validate_windows, GuestWindow, HexBytes, MAX_BIN_CHUNK, MAX_COPY_PATH, MAX_DIMENSION,
    MAX_EXEC_ARG, MAX_EXEC_ARGV, MAX_NAME,
};

/// One protocol message. JSON form: `{"type":"<snake_case name>", ...fields}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// Agent → host, first message on every connection.
    Hello {
        /// Highest protocol version the agent speaks.
        version: u32,
        /// Guest OS description, e.g. `Linux 6.6.58-0-virt aarch64`.
        os: String,
        /// Agent build, e.g. `ternvale-agent 0.1.0`. Optional on the wire.
        #[serde(default)]
        agent: String,
    },
    /// Host → agent: handshake accepted; both sides now speak `version`.
    Welcome {
        /// Negotiated protocol version.
        version: u32,
    },
    /// Host → agent: handshake refused; the host closes the connection.
    Reject {
        /// Why, for the agent's log.
        reason: String,
        /// Oldest version the host accepts.
        min_version: u32,
        /// Newest version the host accepts.
        max_version: u32,
    },
    /// Liveness probe; the receiver answers `Pong` with the same `seq`.
    Ping {
        /// Sender-chosen sequence number.
        seq: u64,
    },
    /// Answer to `Ping`.
    Pong {
        /// The `seq` of the `Ping` being answered.
        seq: u64,
    },
    /// Host → agent: resize the guest display.
    SetResolution {
        /// Pixels.
        width: u32,
        /// Pixels.
        height: u32,
    },
    /// Host → agent: replace the guest clipboard.
    ClipboardSet {
        /// New clipboard contents.
        text: ClipboardText,
    },
    /// Host → agent: read the guest clipboard (answer with `ClipboardGetOk`).
    ClipboardGet,
    /// Agent → host: answer to `ClipboardGet`.
    ClipboardGetOk {
        /// Guest clipboard text (may be empty).
        text: ClipboardText,
    },
    /// Host → agent: mount the virtio-fs share (Linux only).
    ShareMount {
        /// virtio-fs mount tag (matches device config).
        tag: String,
        /// Guest path to mount at.
        mount_point: String,
    },
    /// Host → agent: flush and power the guest off.
    Shutdown,
    /// Host → agent: set guest wall clock after host sleep/wake.
    TimeSync {
        /// Unix seconds (UTC).
        unix_secs: i64,
    },
    /// Host → agent: list mapped top-level windows (answer with `WindowListOk`, v3+).
    WindowListGet,
    /// Agent → host: answer to `WindowListGet`.
    WindowListOk {
        /// Windows in guest screen coordinates.
        windows: Vec<GuestWindow>,
    },
    /// Host → agent: run a command (requires `agent.allow_exec` on the host).
    Exec {
        /// Correlates `ExecOutput` / `ExecExit`.
        id: u64,
        /// Program and arguments (not a shell string).
        argv: Vec<String>,
        /// Kill the process after this many milliseconds. Omitted: host default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Agent → host: one stdout or stderr chunk.
    ExecOutput {
        /// The `Exec` id.
        id: u64,
        /// `stdout` or `stderr`.
        stream: ExecStream,
        /// Chunk bytes (hex on the wire).
        data: HexBytes,
    },
    /// Agent → host: the process exited (or was killed).
    ExecExit {
        /// The `Exec` id.
        id: u64,
        /// Process exit status (`128 + signal` when killed by a signal).
        code: i32,
    },
    /// Host → agent: write `data` at `offset` in `path` (`last` closes the file).
    CopyPut {
        /// Correlates `CopyPutOk` / `CopyErr`.
        id: u64,
        /// Absolute guest path.
        path: String,
        /// Byte offset (0 truncates).
        offset: u64,
        /// Chunk bytes (hex on the wire).
        data: HexBytes,
        /// True on the last chunk.
        last: bool,
    },
    /// Agent → host: `CopyPut` landed.
    CopyPutOk {
        /// The `CopyPut` id.
        id: u64,
        /// Bytes written this chunk.
        written: u64,
    },
    /// Host → agent: read `len` bytes at `offset`.
    CopyGet {
        /// Correlates `CopyGetOk` / `CopyErr`.
        id: u64,
        /// Absolute guest path.
        path: String,
        /// Byte offset.
        offset: u64,
        /// Max bytes to return (capped at [`crate::MAX_BIN_CHUNK`]).
        len: u32,
    },
    /// Agent → host: file bytes (may be shorter than requested).
    CopyGetOk {
        /// The `CopyGet` id.
        id: u64,
        /// Chunk bytes (hex on the wire).
        data: HexBytes,
        /// True when `offset + data` is the end of the file.
        eof: bool,
    },
    /// Host → agent: SHA-256 and size of `path`.
    CopyHash {
        /// Correlates `CopyHashOk` / `CopyErr`.
        id: u64,
        /// Absolute guest path.
        path: String,
    },
    /// Agent → host: SHA-256 of the whole file.
    CopyHashOk {
        /// The `CopyHash` id.
        id: u64,
        /// File size in bytes.
        size: u64,
        /// Lowercase hex SHA-256.
        sha256: String,
    },
    /// Agent → host: copy/exec request failed.
    CopyErr {
        /// The request id.
        id: u64,
        /// Why, for the host log (not a guest secret).
        reason: String,
    },
}

/// Stdout or stderr for [`Message::ExecOutput`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecStream {
    /// Process stdout.
    Stdout,
    /// Process stderr.
    Stderr,
}

impl ExecStream {
    /// Wire / log name.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// Clipboard contents. `Debug` prints only the length, so logging a message
/// never copies the user's clipboard into the logs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClipboardText(pub String);

impl std::fmt::Debug for ClipboardText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "<{} bytes>", self.0.len())
    }
}

/// Absolute guest file path: Unix `/…` or Windows `X:\…` / `X:/…`, no `..`.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn is_guest_file_path(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_COPY_PATH || path.contains('\0') {
        return false;
    }
    if path.split(['/', '\\']).any(|part| part == "..") {
        return false;
    }
    if path.starts_with('/') {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// Linux `/path` or Windows `X:` / `X:\…` (same rule as `ternvale-config`).
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn is_guest_mount_point(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    let bytes = path.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return false;
    }
    let rest = &path[2..];
    rest.is_empty() || rest.starts_with('\\') || rest.starts_with('/')
}

impl Message {
    /// The wire `type` tag.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Welcome { .. } => "welcome",
            Self::Reject { .. } => "reject",
            Self::Ping { .. } => "ping",
            Self::Pong { .. } => "pong",
            Self::SetResolution { .. } => "set_resolution",
            Self::ClipboardSet { .. } => "clipboard_set",
            Self::ClipboardGet => "clipboard_get",
            Self::ClipboardGetOk { .. } => "clipboard_get_ok",
            Self::ShareMount { .. } => "share_mount",
            Self::Shutdown => "shutdown",
            Self::TimeSync { .. } => "time_sync",
            Self::WindowListGet => "window_list_get",
            Self::WindowListOk { .. } => "window_list_ok",
            Self::Exec { .. } => "exec",
            Self::ExecOutput { .. } => "exec_output",
            Self::ExecExit { .. } => "exec_exit",
            Self::CopyPut { .. } => "copy_put",
            Self::CopyPutOk { .. } => "copy_put_ok",
            Self::CopyGet { .. } => "copy_get",
            Self::CopyGetOk { .. } => "copy_get_ok",
            Self::CopyHash { .. } => "copy_hash",
            Self::CopyHashOk { .. } => "copy_hash_ok",
            Self::CopyErr { .. } => "copy_err",
        }
    }

    /// Range checks on peer-supplied fields. `Err` names the problem.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Hello { os, agent, .. } if os.len() > MAX_NAME || agent.len() > MAX_NAME => {
                Err(format!("hello os/agent longer than {MAX_NAME} bytes"))
            }
            Self::Reject { reason, .. } if reason.len() > MAX_NAME => {
                Err(format!("reject reason longer than {MAX_NAME} bytes"))
            }
            Self::SetResolution { width, height }
                if !(1..=MAX_DIMENSION).contains(width)
                    || !(1..=MAX_DIMENSION).contains(height) =>
            {
                Err(format!(
                    "resolution {width}x{height} outside 1..={MAX_DIMENSION}"
                ))
            }
            Self::ShareMount { tag, mount_point } => {
                if tag.is_empty() || tag.len() > crate::MAX_TAG_LEN {
                    Err(format!(
                        "share_mount tag length must be 1..={}",
                        crate::MAX_TAG_LEN
                    ))
                } else if mount_point.is_empty() || mount_point.len() > crate::MAX_MOUNT_PATH {
                    Err(format!(
                        "share_mount mount_point length must be 1..={}",
                        crate::MAX_MOUNT_PATH
                    ))
                } else if !is_guest_mount_point(mount_point) {
                    Err("share_mount mount_point must be an absolute path or a Windows drive letter".into())
                } else {
                    Ok(())
                }
            }
            Self::WindowListOk { windows } => validate_windows(windows),
            Self::Exec { argv, .. } => validate_argv(argv),
            Self::ExecOutput { data, .. } if data.0.len() > MAX_BIN_CHUNK => {
                Err(format!("exec_output longer than {MAX_BIN_CHUNK} bytes"))
            }
            Self::CopyPut { path, data, .. } => validate_copy_path(path).and_then(|()| {
                if data.0.len() > MAX_BIN_CHUNK {
                    Err(format!("copy_put longer than {MAX_BIN_CHUNK} bytes"))
                } else {
                    Ok(())
                }
            }),
            Self::CopyGet { path, len, .. } => validate_copy_path(path).and_then(|()| {
                if *len == 0 || *len as usize > MAX_BIN_CHUNK {
                    Err(format!("copy_get len must be 1..={MAX_BIN_CHUNK}"))
                } else {
                    Ok(())
                }
            }),
            Self::CopyGetOk { data, .. } if data.0.len() > MAX_BIN_CHUNK => {
                Err(format!("copy_get_ok longer than {MAX_BIN_CHUNK} bytes"))
            }
            Self::CopyHash { path, .. } => validate_copy_path(path),
            Self::CopyHashOk { sha256, .. } if sha256.len() != 64 => {
                Err("copy_hash_ok sha256 must be 64 hex chars".into())
            }
            Self::CopyErr { reason, .. } if reason.len() > MAX_NAME => {
                Err(format!("copy_err reason longer than {MAX_NAME} bytes"))
            }
            _ => Ok(()),
        }
    }
}

fn validate_argv(argv: &[String]) -> Result<(), String> {
    if argv.is_empty() || argv.len() > MAX_EXEC_ARGV {
        return Err(format!("exec argv length must be 1..={MAX_EXEC_ARGV}"));
    }
    if argv
        .iter()
        .any(|arg| arg.is_empty() || arg.len() > MAX_EXEC_ARG)
    {
        return Err(format!("exec argument length must be 1..={MAX_EXEC_ARG}"));
    }
    Ok(())
}

fn validate_copy_path(path: &str) -> Result<(), String> {
    if is_guest_file_path(path) {
        Ok(())
    } else {
        Err("copy path must be an absolute guest path without '..'".into())
    }
}
