//! JSON-RPC notifications after `subscribe`.

use serde::{Deserialize, Serialize};

/// Event kinds a client may request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    /// VM lifecycle state (`created`, `running`, `paused`, `stopping`, …).
    State,
    /// Guest agent connected.
    AgentConnect,
    /// Guest agent disconnected.
    AgentDisconnect,
    /// Guest tools older than the attached tools ISO.
    GuestToolsOutdated,
    /// One host log line (already formatted).
    Log,
    /// macOS restore download or `VZMacOSInstaller` progress (`permille` 0..=1000).
    InstallProgress,
}

impl EventKind {
    /// Wire name.
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::AgentConnect => "agent-connect",
            Self::AgentDisconnect => "agent-disconnect",
            Self::GuestToolsOutdated => "guest-tools-outdated",
            Self::Log => "log",
            Self::InstallProgress => "install-progress",
        }
    }

    /// Parse a wire name.
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(name))]
    pub fn parse(name: &str) -> Option<Self> {
        EVENT_KINDS.iter().copied().find(|k| k.as_str() == name)
    }
}

/// Every event kind. Do not reorder.
pub const EVENT_KINDS: &[EventKind] = &[
    EventKind::State,
    EventKind::AgentConnect,
    EventKind::AgentDisconnect,
    EventKind::GuestToolsOutdated,
    EventKind::Log,
    EventKind::InstallProgress,
];

/// Notification params (`method` is always `event`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub kind: EventKind,
    pub vm: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_outdated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
    /// Completed units (`install-progress`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<u64>,
    /// Total units (`install-progress`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Progress in parts per thousand, 0..=1000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permille: Option<u32>,
}

impl Event {
    /// JSON-RPC notification object (no `id`).
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(kind = self.kind.as_str(), vm = self.vm.as_str()))]
    pub fn notification(&self) -> super::JsonRpcRequest {
        super::JsonRpcRequest {
            jsonrpc: super::JSONRPC_VERSION.to_string(),
            method: "event".to_string(),
            params: Some(match serde_json::to_value(self) {
                Ok(value) => value,
                Err(error) => {
                    tracing::error!(
                        target: "ternvale::api",
                        error = %error,
                        "event serialize failed"
                    );
                    serde_json::Value::Null
                }
            }),
            id: None,
        }
    }
}
