//! Append-only JSON-lines audit log (`TVAL` v1 header, then events).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

use crate::error::GatewayError;

/// First-line magic object for the audit log.
pub const AUDIT_HEADER: &str = r#"{"magic":"TVAL","version":1}"#;

/// One audited request (no secrets).
#[derive(Debug, Clone, Serialize)]
pub struct AuditEvent {
    /// RFC 3339 UTC.
    pub ts: String,
    /// Token id or `mtls` subject.
    pub who: String,
    /// Scope name, if known.
    pub scope: String,
    /// HTTP method.
    pub method: String,
    /// Request path (no query secrets expected).
    pub path: String,
    /// HTTP status.
    pub status: u16,
    /// Optional reason (`expired token`, `rate limited`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Append-only audit writer.
pub struct AuditLog {
    path: PathBuf,
    file: Mutex<File>,
}

impl AuditLog {
    /// Open `path`, writing a `TVAL` v1 header if the file is new.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(path = %path.as_ref().display()))]
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GatewayError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| {
                GatewayError::io_path("create audit directory", parent.display(), source)
            })?;
        }
        let existed = path.is_file() && path.metadata().map(|m| m.len() > 0).unwrap_or(false);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| GatewayError::io_path("open audit log", path.display(), source))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|source| GatewayError::io_path("chmod audit log", path.display(), source))?;
        if !existed {
            writeln!(file, "{AUDIT_HEADER}").map_err(|source| {
                GatewayError::io_path("write audit header", path.display(), source)
            })?;
        }
        tracing::info!(
            target: "ternvale::gateway",
            path = %path.display(),
            "audit log open"
        );
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    /// Append one event. Failures are logged; they do not fail the request path
    /// except when the lock is poisoned (process is already dying).
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(who = %event.who, status = event.status))]
    pub fn append(&self, event: &AuditEvent) -> Result<(), GatewayError> {
        let line = serde_json::to_string(event)?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| GatewayError::Config("audit log lock poisoned".into()))?;
        writeln!(&mut *file, "{line}").map_err(|source| {
            GatewayError::io_path("append audit log", self.path.display(), source)
        })?;
        file.flush().map_err(|source| {
            GatewayError::io_path("flush audit log", self.path.display(), source)
        })?;
        Ok(())
    }
}

/// RFC 3339 UTC now.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_then_event() {
        let dir = std::env::temp_dir().join(format!("tval-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("audit.jsonl");
        let log = AuditLog::open(&path).expect("open");
        log.append(&AuditEvent {
            ts: "2026-01-01T00:00:00Z".into(),
            who: "token:abc".into(),
            scope: "read-only".into(),
            method: "GET".into(),
            path: "/v1/vms".into(),
            status: 200,
            reason: None,
        })
        .expect("append");
        let text = std::fs::read_to_string(&path).expect("read");
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some(AUDIT_HEADER));
        assert!(lines.next().expect("event").contains("token:abc"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
