//! Protocol messages.

use serde::{Deserialize, Serialize};

use crate::{
    validate_app_launch, validate_apps, validate_clipboard_offer, validate_clipboard_payload,
    validate_clipboard_take, validate_drag_accept, validate_drag_cancel, validate_drag_offer,
    validate_drag_progress, validate_host_open, validate_printers, validate_window_frame,
    validate_window_input, validate_window_op, validate_windows, GuestApp, GuestPrinter,
    GuestWindow, HexBytes, MAX_BIN_CHUNK, MAX_COPY_PATH, MAX_DIMENSION, MAX_EXEC_ARG,
    MAX_EXEC_ARGV, MAX_NAME,
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
    /// Agent → host: the foreground app wants a relative / captured pointer
    /// (games). Additive; older hosts ignore unknown types, older agents never
    /// send this.
    PointerMode {
        /// Relative motion / capture recommended.
        relative: bool,
    },
    /// Host → agent: list mapped top-level windows (answer with `WindowListOk`, v3+).
    WindowListGet,
    /// Agent → host: answer to `WindowListGet`.
    WindowListOk {
        /// Windows in guest screen coordinates.
        windows: Vec<GuestWindow>,
    },
    /// Host → agent: move / resize / minimize / close / focus a guest window.
    WindowOp {
        /// Guest window id (HWND / X11).
        id: u32,
        /// `move`, `resize`, `minimize`, `maximize`, `restore`, `close`, `focus`.
        op: String,
        /// New left edge (`move`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        /// New top edge (`move`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// New width (`move` / `resize`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        /// New height (`move` / `resize`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
    /// Agent → host: `WindowOp` applied.
    WindowOpOk {
        /// Window id.
        id: u32,
        /// The op that ran.
        op: String,
    },
    /// Agent → host: `WindowOp` failed.
    WindowOpErr {
        /// Window id.
        id: u32,
        /// Why (not a secret).
        reason: String,
    },
    /// Host → agent: inject input into a specific guest window.
    WindowInput {
        /// Target window id.
        id: u32,
        /// `key`, `button`, `move`, `abs`.
        kind: String,
        /// Linux KEY_/BTN_ code or 0.
        code: u32,
        /// Press (1) / release (0) or coordinate.
        value: i32,
    },
    /// Agent → host: one chunk of a per-window capture (never log `data`).
    ///
    /// TODO(verify): a dedicated vsock or shared-memory ring would be better
    /// than JSON-hex for full-rate Windows Graphics Capture.
    WindowFrame {
        /// Guest window id.
        id: u32,
        /// Frame sequence for this window.
        seq: u64,
        /// Byte offset in the assembled buffer.
        offset: u64,
        /// Last chunk of this `seq`.
        last: bool,
        /// Pixels.
        width: u32,
        /// Pixels.
        height: u32,
        /// `rgb24`.
        format: String,
        /// Chunk bytes (hex on the wire).
        data: HexBytes,
    },
    /// Host → agent: capture chunk landed (optional).
    WindowFrameOk {
        /// Window id.
        id: u32,
        /// The `seq` of [`Self::WindowFrame`].
        seq: u64,
        /// Bytes written this chunk.
        written: u64,
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
    /// Host → agent: pull KMDF/WPP text logs written under ProgramData.
    DriverLogsGet {
        /// Cap (bytes). Omitted: [`crate::MAX_DRIVER_LOG_BYTES`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_bytes: Option<u32>,
    },
    /// Agent → host: concatenated UTF-8 driver logs.
    DriverLogsOk {
        /// Log text (may be empty if nothing is installed yet).
        text: String,
        /// True when the guest truncated to `max_bytes`.
        truncated: bool,
    },
    /// Host → agent: one chunk of a webcam frame (never log `data`).
    CameraFrame {
        /// Frame sequence.
        seq: u64,
        /// Byte offset in the assembled frame.
        offset: u64,
        /// Last chunk of this `seq`.
        last: bool,
        /// Pixels.
        width: u32,
        /// Pixels.
        height: u32,
        /// `rgb24`.
        format: String,
        /// Chunk bytes (hex on the wire).
        data: HexBytes,
    },
    /// Agent → host: camera chunk landed (optional).
    CameraFrameOk {
        /// The `seq` of [`Self::CameraFrame`].
        seq: u64,
        /// Bytes written this chunk.
        written: u64,
    },
    /// Host → agent: list Start-menu / packaged apps (answer `AppListOk`).
    AppListGet,
    /// Agent → host: Start-menu shortcuts and packaged apps.
    AppListOk {
        /// Apps (capped).
        apps: Vec<GuestApp>,
    },
    /// Host → agent: start a listed app, optionally with a guest file path.
    AppLaunch {
        /// [`GuestApp::id`].
        id: String,
        /// Absolute guest path on a share (Open With). Omitted: launch only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
    },
    /// Agent → host: `AppLaunch` started.
    AppLaunchOk {
        /// App id.
        id: String,
    },
    /// Agent → host: `AppLaunch` failed.
    AppLaunchErr {
        /// App id.
        id: String,
        /// Why (not a secret).
        reason: String,
    },
    /// Agent → host: open a URL or share-mapped file on the host.
    HostOpen {
        /// `url` or `file`.
        kind: String,
        /// URL or absolute guest path.
        target: String,
    },
    /// Host → agent: `HostOpen` accepted.
    HostOpenOk {
        /// Echo of `kind`.
        kind: String,
    },
    /// Host → agent: `HostOpen` refused.
    HostOpenErr {
        /// Echo of `kind`.
        kind: String,
        /// Why (not a secret).
        reason: String,
    },
    /// Either side: offer clipboard formats (loop-prevention `echo`).
    ClipboardOffer {
        /// Offer sequence.
        seq: u64,
        /// Format names (`png`, `dib`, `html`, `rtf`, `text`, `files`).
        formats: Vec<String>,
        /// Byte sizes parallel to `formats` (empty = unknown).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sizes: Vec<u64>,
        /// File names when `files` is offered.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<String>,
        /// Token from the last set we originated (0 = none).
        echo: u64,
    },
    /// Either side: take one format from the last offer.
    ClipboardTake {
        /// Offer sequence.
        seq: u64,
        /// Chosen format.
        format: String,
    },
    /// Either side: one chunk of a rich clipboard payload (never log `data`).
    ClipboardPayload {
        /// Offer sequence.
        seq: u64,
        /// Format of this payload.
        format: String,
        /// Byte offset in the assembled blob.
        offset: u64,
        /// Last chunk.
        last: bool,
        /// Chunk bytes (hex on the wire).
        data: HexBytes,
        /// Echo token (same as the offer).
        echo: u64,
    },
    /// Either side: chunk landed.
    ClipboardPayloadOk {
        /// Offer sequence.
        seq: u64,
        /// Bytes written this chunk.
        written: u64,
    },
    /// Either side: file list about to move (Finder ↔ guest).
    DragOffer {
        /// Transfer id.
        id: String,
        /// Suggested file names (unsanitized; host sanitizes).
        names: Vec<String>,
        /// Sizes parallel to `names` (empty = unknown).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sizes: Vec<u64>,
        /// True: guest → host. False: host → guest.
        outbound: bool,
        /// Echo token (0 = none).
        echo: u64,
    },
    /// Either side: accept a drag into `dest` (empty = default drop folder).
    DragAccept {
        /// Transfer id.
        id: String,
        /// Staging / drop directory hint.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        dest: String,
    },
    /// Either side: abort a transfer.
    DragCancel {
        /// Transfer id.
        id: String,
        /// Why.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        reason: String,
    },
    /// Either side: copy progress.
    DragProgress {
        /// Transfer id.
        id: String,
        /// Bytes copied.
        done: u64,
        /// Total bytes (0 = unknown).
        total: u64,
    },
    /// Either side: transfer finished.
    DragDone {
        /// Transfer id.
        id: String,
    },
    /// Host → agent: install or remove virtual printers (empty list uninstalls).
    PrintersSet {
        /// Printers the guest should expose.
        printers: Vec<GuestPrinter>,
    },
    /// Agent → host: printers applied.
    PrintersSetOk {
        /// How many printers are installed.
        count: u32,
    },
    /// Agent → host: install failed.
    PrintersSetErr {
        /// Why (not a secret).
        reason: String,
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
            Self::PointerMode { .. } => "pointer_mode",
            Self::WindowListGet => "window_list_get",
            Self::WindowListOk { .. } => "window_list_ok",
            Self::WindowOp { .. } => "window_op",
            Self::WindowOpOk { .. } => "window_op_ok",
            Self::WindowOpErr { .. } => "window_op_err",
            Self::WindowInput { .. } => "window_input",
            Self::WindowFrame { .. } => "window_frame",
            Self::WindowFrameOk { .. } => "window_frame_ok",
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
            Self::DriverLogsGet { .. } => "driver_logs_get",
            Self::DriverLogsOk { .. } => "driver_logs_ok",
            Self::CameraFrame { .. } => "camera_frame",
            Self::CameraFrameOk { .. } => "camera_frame_ok",
            Self::AppListGet => "app_list_get",
            Self::AppListOk { .. } => "app_list_ok",
            Self::AppLaunch { .. } => "app_launch",
            Self::AppLaunchOk { .. } => "app_launch_ok",
            Self::AppLaunchErr { .. } => "app_launch_err",
            Self::HostOpen { .. } => "host_open",
            Self::HostOpenOk { .. } => "host_open_ok",
            Self::HostOpenErr { .. } => "host_open_err",
            Self::ClipboardOffer { .. } => "clipboard_offer",
            Self::ClipboardTake { .. } => "clipboard_take",
            Self::ClipboardPayload { .. } => "clipboard_payload",
            Self::ClipboardPayloadOk { .. } => "clipboard_payload_ok",
            Self::DragOffer { .. } => "drag_offer",
            Self::DragAccept { .. } => "drag_accept",
            Self::DragCancel { .. } => "drag_cancel",
            Self::DragProgress { .. } => "drag_progress",
            Self::DragDone { .. } => "drag_done",
            Self::PrintersSet { .. } => "printers_set",
            Self::PrintersSetOk { .. } => "printers_set_ok",
            Self::PrintersSetErr { .. } => "printers_set_err",
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
            Self::WindowOp {
                id,
                op,
                width,
                height,
                ..
            } => validate_window_op(*id, op, *width, *height),
            Self::WindowOpErr { reason, .. } if reason.len() > MAX_NAME => {
                Err(format!("window_op_err reason longer than {MAX_NAME} bytes"))
            }
            Self::WindowInput { id, kind, .. } => validate_window_input(*id, kind),
            Self::WindowFrame {
                id,
                offset,
                width,
                height,
                format,
                data,
                ..
            } => validate_window_frame(*id, *offset, *width, *height, format, data.0.len()),
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
            Self::DriverLogsGet { max_bytes: Some(n) }
                if *n == 0 || *n as usize > crate::MAX_DRIVER_LOG_BYTES =>
            {
                Err(format!(
                    "driver_logs_get max_bytes must be 1..={}",
                    crate::MAX_DRIVER_LOG_BYTES
                ))
            }
            Self::DriverLogsOk { text, .. } if text.len() > crate::MAX_DRIVER_LOG_BYTES => {
                Err(format!(
                    "driver_logs_ok longer than {} bytes",
                    crate::MAX_DRIVER_LOG_BYTES
                ))
            }
            Self::CameraFrame {
                width,
                height,
                data,
                format,
                ..
            } => {
                if *width == 0 || *height == 0 || *width > MAX_DIMENSION || *height > MAX_DIMENSION
                {
                    Err("camera_frame geometry out of range".into())
                } else if format != "rgb24" {
                    Err("camera_frame format must be rgb24".into())
                } else if data.0.len() > MAX_BIN_CHUNK {
                    Err(format!("camera_frame longer than {MAX_BIN_CHUNK} bytes"))
                } else {
                    Ok(())
                }
            }
            Self::AppListOk { apps } => validate_apps(apps),
            Self::AppLaunch { id, file } => validate_app_launch(id, file.as_deref()),
            Self::AppLaunchOk { id } => validate_app_launch(id, None),
            Self::AppLaunchErr { id, reason } => validate_app_launch(id, None).and_then(|()| {
                if reason.len() > MAX_NAME {
                    Err(format!(
                        "app_launch_err reason longer than {MAX_NAME} bytes"
                    ))
                } else {
                    Ok(())
                }
            }),
            Self::HostOpen { kind, target } => validate_host_open(kind, target),
            Self::HostOpenErr { reason, .. } if reason.len() > MAX_NAME => {
                Err(format!("host_open_err reason longer than {MAX_NAME} bytes"))
            }
            Self::ClipboardOffer {
                formats,
                sizes,
                files,
                ..
            } => validate_clipboard_offer(formats, sizes, files),
            Self::ClipboardTake { format, .. } => validate_clipboard_take(format),
            Self::ClipboardPayload { format, data, .. } => {
                validate_clipboard_payload(format, data.0.len())
            }
            Self::DragOffer {
                id, names, sizes, ..
            } => validate_drag_offer(id, names, sizes),
            Self::DragAccept { id, dest } => validate_drag_accept(id, dest),
            Self::DragCancel { id, reason } => validate_drag_cancel(id, reason),
            Self::DragProgress { id, done, total } => validate_drag_progress(id, *done, *total),
            Self::DragDone { id } => crate::validate_drag_id(id),
            Self::PrintersSet { printers } => validate_printers(printers),
            Self::PrintersSetErr { reason } if reason.len() > MAX_NAME => Err(format!(
                "printers_set_err reason longer than {MAX_NAME} bytes"
            )),
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
