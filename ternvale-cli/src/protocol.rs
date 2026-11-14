//! Control socket protocol: one JSON object per line in each direction.
//!
//! Requests: `{"cmd":"status"}`, `{"cmd":"pause","timeout_ms":5000}`,
//! `{"cmd":"resume"}`, `{"cmd":"shutdown"}`, `{"cmd":"force-stop"}`,
//! `{"cmd":"query-stats"}`, `{"cmd":"dump-diagnostics"}`. Every response has
//! `ok`; success carries `status` (and `stats` for `query-stats`, `files` for
//! `dump-diagnostics`), failure carries `error`.
//! `status.agent` describes the guest agent connection when the VM runs an
//! agent server.

use serde::{Deserialize, Serialize};
use ternvale_devices::{AgentStatus, BalloonSnapshot};
use ternvale_vmm::{VmStats, VmStatus};

/// Hex-encoded DEK on `set-data-key`. `Debug` is `[redacted]`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataKeyHex(pub String);

impl std::fmt::Debug for DataKeyHex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[redacted]")
    }
}

/// Longest request line accepted, in bytes.
pub const MAX_LINE: usize = 64 * 1024;
/// Default and largest `pause` wait.
pub const DEFAULT_PAUSE_MS: u64 = 5_000;
/// Upper bound on a client-supplied `timeout_ms`.
pub const MAX_PAUSE_MS: u64 = 60_000;

/// One control request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    /// Current state.
    Status,
    /// Park every vCPU.
    Pause {
        /// How long to wait for vCPUs to leave the guest.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Unpark.
    Resume,
    /// Orderly stop.
    Shutdown,
    /// Stop; the VM process exits even if teardown hangs.
    ForceStop,
    /// Pulse the guest power button and wait; force-stop after `timeout_ms`.
    GracefulStop {
        /// How long to wait for the guest OS (milliseconds).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// App display visibility (`pause_when_hidden` / `low_power`).
    WindowHidden {
        /// True when the VM window is not visible.
        hidden: bool,
    },
    /// State plus per-vCPU counters.
    QueryStats,
    /// Write the guest DTB, MMIO events, and summary beside the host log.
    DumpDiagnostics,
    /// Pause, capture guest RAM/GIC/vCPU state, write a bundle, resume.
    SnapshotCreate {
        /// Snapshot label (directory name under the VM's snapshot store).
        name: String,
    },
    /// Pause, restore a bundle, resume.
    SnapshotRestore {
        /// Snapshot label.
        name: String,
    },
    /// List snapshot labels for this VM (no pause).
    SnapshotList,
    /// Delete a snapshot bundle directory.
    SnapshotDelete {
        /// Snapshot label.
        name: String,
    },
    /// Guest clipboard → host pasteboard (and response `clipboard_text`).
    ClipboardGet {
        /// How long to wait for the guest agent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Host pasteboard + guest clipboard from `text`.
    ClipboardSet {
        /// New clipboard contents.
        text: String,
    },
    /// Set virtio-balloon `num_pages` (`target_mib`) or resume auto policy.
    BalloonSet {
        /// Requested balloon size in MiB (capped by floor and guest in-use).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_mib: Option<u64>,
        /// When true, drop a manual target and follow host memory pressure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto: Option<bool>,
    },
    /// Unwrapped 32-byte data key as hex. Never put this on argv or in the environment.
    SetDataKey {
        /// Lowercase hex of the 32-byte DEK.
        key_hex: DataKeyHex,
    },
    /// Run `argv` in the guest (`agent.allow_exec`).
    Exec {
        /// Program and arguments (not a shell).
        argv: Vec<String>,
        /// Kill after this many milliseconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Copy a file between host and guest (`agent.allow_exec`).
    Copy {
        /// Absolute guest path.
        guest_path: String,
        /// Host path (must exist for host→guest).
        host_path: String,
        /// True: host file → guest. False: guest → host.
        to_guest: bool,
        /// Resume from this byte offset.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<u64>,
        /// Copy at most this many bytes from `offset` (CLI slices so a kill can resume).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u64>,
    },
    /// Grow a virtio-blk image and raise a config-change interrupt.
    DiskResize {
        /// New size in bytes (sector-aligned by the VMM).
        bytes: u64,
        /// Allow shrinking below the in-use size.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        force: bool,
        /// `[[disks]]` index (default 0).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disk: Option<u32>,
    },
    /// Compact is refused while the VM is running.
    DiskCompact {
        /// `[[disks]]` index (default 0).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disk: Option<u32>,
    },
}

impl Request {
    /// The `cmd` string.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Pause { .. } => "pause",
            Self::Resume => "resume",
            Self::Shutdown => "shutdown",
            Self::ForceStop => "force-stop",
            Self::GracefulStop { .. } => "graceful-stop",
            Self::WindowHidden { .. } => "window-hidden",
            Self::QueryStats => "query-stats",
            Self::DumpDiagnostics => "dump-diagnostics",
            Self::SnapshotCreate { .. } => "snapshot-create",
            Self::SnapshotRestore { .. } => "snapshot-restore",
            Self::SnapshotList => "snapshot-list",
            Self::SnapshotDelete { .. } => "snapshot-delete",
            Self::ClipboardGet { .. } => "clipboard-get",
            Self::ClipboardSet { .. } => "clipboard-set",
            Self::BalloonSet { .. } => "balloon-set",
            Self::SetDataKey { .. } => "set-data-key",
            Self::Exec { .. } => "exec",
            Self::Copy { .. } => "copy",
            Self::DiskResize { .. } => "disk-resize",
            Self::DiskCompact { .. } => "disk-compact",
        }
    }

    /// What the user asked for, as a verb for error messages.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Status | Self::QueryStats => "query",
            Self::DumpDiagnostics => "dump diagnostics for",
            Self::Pause { .. } => "pause",
            Self::Resume => "resume",
            Self::Shutdown => "stop",
            Self::ForceStop => "force-stop",
            Self::GracefulStop { .. } => "gracefully stop",
            Self::WindowHidden { .. } => "update window visibility for",
            Self::SnapshotCreate { .. } => "snapshot create for",
            Self::SnapshotRestore { .. } => "snapshot restore for",
            Self::SnapshotList => "list snapshots for",
            Self::SnapshotDelete { .. } => "snapshot delete for",
            Self::ClipboardGet { .. } => "clipboard get for",
            Self::ClipboardSet { .. } => "clipboard set for",
            Self::BalloonSet { .. } => "set balloon for",
            Self::SetDataKey { .. } => "set data key for",
            Self::Exec { .. } => "exec on",
            Self::Copy { .. } => "copy for",
            Self::DiskResize { .. } => "resize disk for",
            Self::DiskCompact { .. } => "compact disk for",
        }
    }
}

/// `status` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusJson {
    pub name: String,
    pub state: String,
    pub cpus: u32,
    pub uptime_ms: u64,
    pub state_ms: u64,
    pub paused_ms: u64,
    pub pauses: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_cause: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// Guest agent connection; absent when the VM runs no agent server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentJson>,
    /// Guest tools older than the attached `ternvale-tools.iso` (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_outdated: Option<bool>,
    /// `agent_version` from the tools ISO manifest (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_iso_agent: Option<String>,
    /// Resolved hypervisor backend (`native` / `vz`). Optional for older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Capability flags for `backend`. Optional for older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ternvale_backend::BackendCapabilities>,
    /// Resolved x86 usermode engine (`none` / `rosetta` / `fex` / `qemu-user`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x86_engine: Option<String>,
}

/// Guest agent connection in `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentJson {
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pong_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_us: Option<u64>,
    pub connects: u64,
    pub disconnects: u64,
    pub pings: u64,
    pub pongs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl From<&AgentStatus> for AgentJson {
    fn from(status: &AgentStatus) -> Self {
        Self {
            state: status.state.as_str().to_string(),
            version: status.version,
            os: status.os.clone(),
            agent: status.agent.clone(),
            connected_ms: status.connected_ms,
            last_pong_ms: status.last_pong_ms,
            rtt_us: status.rtt_us,
            connects: status.connects,
            disconnects: status.disconnects,
            pings: status.pings,
            pongs: status.pongs,
            last_error: status.last_error.clone(),
        }
    }
}

/// One vCPU in `query-stats`. Counters are absent until the vCPU exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuJson {
    pub cpu: u32,
    pub in_guest: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wfi_parks: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub park_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vtimer_exits: Option<u64>,
}

/// `query-stats` payload beside `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsJson {
    pub cpus: Vec<CpuJson>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_cpu_ms: Option<u64>,
    /// Lock-free VM metrics. Optional so older clients ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<ternvale_metrics::Snapshot>,
    /// virtio-balloon size and guest memory stats. Optional so older clients ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balloon: Option<BalloonSnapshot>,
}

/// One library VM in a `list` result (optional field; older clients ignore it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmListEntry {
    pub name: String,
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Resolved backend name. Optional for older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Capability flags. Optional for older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ternvale_backend::BackendCapabilities>,
}

/// One response line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusJson>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<StatsJson>,
    /// Files written by `dump-diagnostics`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    /// Snapshot labels (`snapshot-list`) or bundle path (`snapshot-create`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshots: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_path: Option<String>,
    /// Plain text from `clipboard-get`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard_text: Option<String>,
    /// Set on JSON-RPC `handshake` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_api_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_api_version: Option<u32>,
    /// `list` / `create` / `delete` payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vms: Option<Vec<VmListEntry>>,
    /// `subscribe` acknowledgement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscribed: Option<Vec<String>>,
    /// `exec` stream name (`stdout` / `stderr`) on a streaming line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,
    /// `exec` chunk as UTF-8 (lossy) on a streaming line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// Guest process exit code (`exec`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Bytes transferred (`copy`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// SHA-256 hex of the copied file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// True when `copy` resumed from a non-zero offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed: Option<bool>,
    /// Host allocated bytes before compact/resize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocated_before: Option<u64>,
    /// Host allocated bytes after compact/resize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocated_after: Option<u64>,
    /// Guest-visible image size after resize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_bytes: Option<u64>,
}

impl Response {
    fn blank(ok: bool) -> Self {
        Self {
            ok,
            error: None,
            status: None,
            stats: None,
            files: None,
            snapshots: None,
            snapshot_path: None,
            clipboard_text: None,
            api_version: None,
            min_api_version: None,
            max_api_version: None,
            vms: None,
            subscribed: None,
            stream: None,
            data: None,
            exit_code: None,
            bytes: None,
            sha256: None,
            resumed: None,
            allocated_before: None,
            allocated_after: None,
            logical_bytes: None,
        }
    }

    /// Success with a status.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn status(status: &VmStatus) -> Self {
        Self {
            status: Some(StatusJson::from(status)),
            ..Self::blank(true)
        }
    }

    /// JSON-RPC handshake payload (optional fields; legacy clients ignore them).
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn handshake(api_version: u32, min: u32, max: u32) -> Self {
        Self {
            api_version: Some(api_version),
            min_api_version: Some(min),
            max_api_version: Some(max),
            ..Self::blank(true)
        }
    }

    /// `list` result.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(n = vms.len()))]
    pub fn vms(vms: Vec<VmListEntry>) -> Self {
        Self {
            vms: Some(vms),
            ..Self::blank(true)
        }
    }

    /// `subscribe` acknowledgement.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn subscribed(kinds: Vec<String>) -> Self {
        Self {
            subscribed: Some(kinds),
            ..Self::blank(true)
        }
    }

    /// Success with guest clipboard text.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn clipboard(status: &VmStatus, text: String) -> Self {
        Self {
            clipboard_text: Some(text),
            ..Self::status(status)
        }
    }

    /// Success with a status and the files a dump wrote.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(files = files.len()))]
    pub fn files(status: &VmStatus, files: Vec<String>) -> Self {
        Self {
            files: Some(files),
            ..Self::status(status)
        }
    }

    /// Success with status and per-vCPU counters.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn stats(stats: &VmStats, balloon: Option<BalloonSnapshot>) -> Self {
        let cpus = stats
            .cpus
            .iter()
            .map(|cpu| CpuJson {
                cpu: cpu.cpu,
                in_guest: cpu.in_guest,
                runs: cpu.stats.map(|s| s.runs),
                guest_ms: cpu.stats.map(|s| s.guest_ms),
                wfi_parks: cpu.stats.map(|s| s.wfi_parks),
                park_ms: cpu.stats.map(|s| s.park_ms),
                vtimer_exits: cpu.stats.map(|s| s.vtimer_exits),
            })
            .collect();
        Self {
            stats: Some(StatsJson {
                cpus,
                process_cpu_ms: stats.process_cpu_ms,
                metrics: Some(stats.metrics.clone()),
                balloon,
            }),
            ..Self::status(&stats.status)
        }
    }

    /// Success with snapshot names or a bundle path.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn snapshots(status: &VmStatus, names: Vec<String>, path: Option<String>) -> Self {
        Self {
            snapshots: Some(names),
            snapshot_path: path,
            ..Self::status(status)
        }
    }

    /// One `exec` stdout/stderr line.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn exec_chunk(stream: &str, data: String) -> Self {
        Self {
            stream: Some(stream.to_string()),
            data: Some(data),
            ..Self::blank(true)
        }
    }

    /// Final `exec` line with the guest exit code.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn exec_exit(code: i32) -> Self {
        Self {
            exit_code: Some(code),
            ..Self::blank(true)
        }
    }

    /// `copy` finished.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn copy_done(bytes: u64, sha256: String, resumed: bool) -> Self {
        Self {
            bytes: Some(bytes),
            sha256: Some(sha256),
            resumed: Some(resumed),
            ..Self::blank(true)
        }
    }

    /// `copy` wrote a slice; `sha256` is omitted until the whole file is hashed.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn copy_progress(bytes: u64, resumed: bool) -> Self {
        Self {
            bytes: Some(bytes),
            resumed: Some(resumed),
            ..Self::blank(true)
        }
    }

    /// Compact/resize allocated sizes (optional fields; older clients ignore them).
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn disk_sizes(before: u64, after: u64, logical: Option<u64>) -> Self {
        Self {
            allocated_before: Some(before),
            allocated_after: Some(after),
            logical_bytes: logical,
            ..Self::blank(true)
        }
    }

    /// Failure with a message.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            error: Some(message.into()),
            ..Self::blank(false)
        }
    }
}

impl From<&VmStatus> for StatusJson {
    fn from(status: &VmStatus) -> Self {
        Self {
            name: status.name.clone(),
            state: status.state.as_str().to_string(),
            cpus: status.cpus,
            uptime_ms: status.uptime_ms,
            state_ms: status.state_ms,
            paused_ms: status.paused_ms,
            pauses: status.pauses,
            stop_cause: status.stop_cause.map(|cause| cause.to_string()),
            failure: status.failure.clone(),
            agent: None,
            tools_outdated: None,
            tools_iso_agent: None,
            backend: None,
            capabilities: None,
            x86_engine: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternvale_vmm::VmControl;

    #[test]
    fn parses_every_request_and_rejects_unknown_ones() {
        let cases = [
            (r#"{"cmd":"status"}"#, Request::Status),
            (r#"{"cmd":"pause"}"#, Request::Pause { timeout_ms: None }),
            (
                r#"{"cmd":"pause","timeout_ms":250}"#,
                Request::Pause {
                    timeout_ms: Some(250),
                },
            ),
            (r#"{"cmd":"resume"}"#, Request::Resume),
            (r#"{"cmd":"shutdown"}"#, Request::Shutdown),
            (r#"{"cmd":"force-stop"}"#, Request::ForceStop),
            (
                r#"{"cmd":"graceful-stop","timeout_ms":5000}"#,
                Request::GracefulStop {
                    timeout_ms: Some(5000),
                },
            ),
            (
                r#"{"cmd":"window-hidden","hidden":true}"#,
                Request::WindowHidden { hidden: true },
            ),
            (r#"{"cmd":"query-stats"}"#, Request::QueryStats),
            (r#"{"cmd":"dump-diagnostics"}"#, Request::DumpDiagnostics),
            (
                r#"{"cmd":"clipboard-get","timeout_ms":1000}"#,
                Request::ClipboardGet {
                    timeout_ms: Some(1000),
                },
            ),
            (
                r#"{"cmd":"clipboard-set","text":"hi"}"#,
                Request::ClipboardSet { text: "hi".into() },
            ),
            (
                r#"{"cmd":"balloon-set","target_mib":1024}"#,
                Request::BalloonSet {
                    target_mib: Some(1024),
                    auto: None,
                },
            ),
            (
                r#"{"cmd":"balloon-set","auto":true}"#,
                Request::BalloonSet {
                    target_mib: None,
                    auto: Some(true),
                },
            ),
            (
                r#"{"cmd":"set-data-key","key_hex":"aa"}"#,
                Request::SetDataKey {
                    key_hex: DataKeyHex("aa".into()),
                },
            ),
            (
                r#"{"cmd":"disk-resize","bytes":17179869184}"#,
                Request::DiskResize {
                    bytes: 17_179_869_184,
                    force: false,
                    disk: None,
                },
            ),
            (
                r#"{"cmd":"disk-compact"}"#,
                Request::DiskCompact { disk: None },
            ),
        ];
        for (line, want) in cases {
            let got: Request = serde_json::from_str(line).expect(line);
            assert_eq!(got, want, "{line}");
            assert_eq!(serde_json::to_string(&got).expect("encode"), line);
        }
        for bad in [
            r#"{"cmd":"reboot"}"#,
            r#"{"cmd":"pause","x":1}"#,
            "status",
            "{}",
        ] {
            assert!(serde_json::from_str::<Request>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn set_data_key_debug_never_prints_hex() {
        let hex = "aa".repeat(32);
        let req = Request::SetDataKey {
            key_hex: DataKeyHex(hex.clone()),
        };
        let debug = format!("{req:?}");
        assert!(
            debug.contains("[redacted]"),
            "expected redacted Debug: {debug}"
        );
        assert!(
            !debug.contains(&hex),
            "set-data-key Debug leaked key hex: {debug}"
        );
        tracing::info!(target: "ternvale::crypto", request = ?req, "control set-data-key");
    }

    #[test]
    fn responses_carry_status_stats_or_an_error() {
        let control = VmControl::new("p", 2);
        let line = serde_json::to_string(&Response::status(&control.status())).expect("encode");
        assert!(
            line.starts_with(r#"{"ok":true,"status":{"name":"p","state":"created","cpus":2"#),
            "{line}"
        );
        let stats = Response::stats(&control.stats(), None);
        assert_eq!(stats.stats.as_ref().map(|s| s.cpus.len()), Some(2));
        assert_eq!(stats.stats.as_ref().and_then(|s| s.cpus[0].runs), None);
        let error = serde_json::to_string(&Response::error("nope")).expect("encode");
        assert_eq!(error, r#"{"ok":false,"error":"nope"}"#);
    }

    fn golden(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("goldens")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn assert_golden(name: &str, value: &impl serde::Serialize) {
        let got = serde_json::to_string(value).expect("enc");
        let want: serde_json::Value = serde_json::from_str(golden(name).trim()).expect("want json");
        let got_v: serde_json::Value = serde_json::from_str(&got).expect("got json");
        assert_eq!(got_v, want, "{name} got={got}");
    }

    #[test]
    fn golden_files_freeze_legacy_cmd_and_error_shapes() {
        assert_golden("cmd-status.json", &Request::Status);
        assert_golden("cmd-pause.json", &Request::Pause { timeout_ms: None });
        assert_golden("cmd-query-stats.json", &Request::QueryStats);
        assert_golden(
            "cmd-balloon-set.json",
            &Request::BalloonSet {
                target_mib: Some(1024),
                auto: None,
            },
        );
        assert_golden(
            "cmd-set-data-key.json",
            &Request::SetDataKey {
                key_hex: DataKeyHex("aa".into()),
            },
        );
        assert_golden("response-error.json", &Response::error("nope"));
        assert_golden(
            "cmd-disk-resize.json",
            &Request::DiskResize {
                bytes: 17_179_869_184,
                force: false,
                disk: None,
            },
        );
        assert_golden(
            "cmd-disk-compact.json",
            &Request::DiskCompact { disk: None },
        );
    }

    #[test]
    fn v1_response_shape_accepts_new_optional_fields() {
        let line = r#"{"ok":true,"error":null,"build":"future","vms":[]}"#;
        let parsed: Response = serde_json::from_str(line).expect("v1 client");
        assert!(parsed.ok);
        assert!(parsed.error.is_none());
        assert!(parsed.vms.is_none() || parsed.vms.as_ref().is_some_and(Vec::is_empty));
    }
}
