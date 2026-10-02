//! Control socket protocol: one JSON object per line in each direction.
//!
//! Requests: `{"cmd":"status"}`, `{"cmd":"pause","timeout_ms":5000}`,
//! `{"cmd":"resume"}`, `{"cmd":"shutdown"}`, `{"cmd":"force-stop"}`,
//! `{"cmd":"query-stats"}`. Every response has `ok`; success carries
//! `status` (and `stats` for `query-stats`), failure carries `error`.
//! `status.agent` describes the guest agent connection when the VM runs an
//! agent server.

use serde::{Deserialize, Serialize};
use ternvale_devices::AgentStatus;
use ternvale_vmm::{VmStats, VmStatus};

/// Longest request line accepted, in bytes.
pub const MAX_LINE: usize = 4096;
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
    /// State plus per-vCPU counters.
    QueryStats,
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
            Self::QueryStats => "query-stats",
        }
    }

    /// What the user asked for, as a verb for error messages.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Status | Self::QueryStats => "query",
            Self::Pause { .. } => "pause",
            Self::Resume => "resume",
            Self::Shutdown => "stop",
            Self::ForceStop => "force-stop",
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
}

impl Response {
    /// Success with a status.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn status(status: &VmStatus) -> Self {
        Self {
            ok: true,
            error: None,
            status: Some(StatusJson::from(status)),
            stats: None,
        }
    }

    /// Success with status and per-vCPU counters.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn stats(stats: &VmStats) -> Self {
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
            ok: true,
            error: None,
            status: Some(StatusJson::from(&stats.status)),
            stats: Some(StatsJson {
                cpus,
                process_cpu_ms: stats.process_cpu_ms,
            }),
        }
    }

    /// Failure with a message.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
            status: None,
            stats: None,
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
            (r#"{"cmd":"query-stats"}"#, Request::QueryStats),
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
    fn responses_carry_status_stats_or_an_error() {
        let control = VmControl::new("p", 2);
        let line = serde_json::to_string(&Response::status(&control.status())).expect("encode");
        assert!(
            line.starts_with(r#"{"ok":true,"status":{"name":"p","state":"created","cpus":2"#),
            "{line}"
        );
        let stats = Response::stats(&control.stats());
        assert_eq!(stats.stats.as_ref().map(|s| s.cpus.len()), Some(2));
        assert_eq!(stats.stats.as_ref().and_then(|s| s.cpus[0].runs), None);
        let error = serde_json::to_string(&Response::error("nope")).expect("encode");
        assert_eq!(error, r#"{"ok":false,"error":"nope"}"#);
    }
}
