//! Guest-side drag-and-drop: stage files, OLE/XDND stubs, harness analog.

use std::path::{Path, PathBuf};

use ternvale_agent_proto::validate_drag_id;

use crate::AgentError;

#[cfg(target_os = "linux")]
const HARNESS_DROP: &str = "/tmp/ternvale-dnd-in";

/// Where inbound files land until the guest shell injects them.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn stage_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TERNVALE_DND_STAGE") {
        return PathBuf::from(dir);
    }
    std::env::temp_dir().join("ternvale-dnd")
}

/// Create the stage directory.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn ensure_stage() -> Result<PathBuf, AgentError> {
    let dir = stage_dir();
    std::fs::create_dir_all(&dir).map_err(|source| AgentError::Action {
        action: "create dnd stage",
        source,
    })?;
    Ok(dir)
}

/// Remove a finished or cancelled transfer directory.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id))]
pub fn cleanup(id: &str) -> Result<(), AgentError> {
    validate_drag_id(id).map_err(|reason| AgentError::Action {
        action: "dnd cleanup",
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, reason),
    })?;
    let dir = stage_dir().join(id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|source| AgentError::Action {
            action: "remove dnd stage",
            source,
        })?;
        tracing::info!(target: "ternvale::agent", id, path = %dir.display(), "dnd stage cleaned");
    }
    Ok(())
}

/// Record a drop for the Linux harness (Step 114 analog).
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id, count = names.len()))]
pub fn harness_record_drop(id: &str, names: &[String]) {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("TERNVALE_AGENT_HARNESS").is_none() {
            return;
        }
        let line = format!("{id}\t{}\n", names.join("\t"));
        if let Err(source) = std::fs::write(HARNESS_DROP, line) {
            tracing::warn!(target: "ternvale::agent", error = %source, "harness dnd write failed");
        }
    }
    let _ = (id, names);
}

/// Inject a drop into the foreground window.
///
/// Windows: OLE `IDropTarget::Drop` (MSDN IDropTarget, CF_HDROP). Live path is
/// TODO(verify) on ARM64 Windows 11.
/// Linux: XDND (`XdndEnter` / `XdndDrop`, freedesktop XDND spec). TODO(verify)
/// against mutter/kwin.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id))]
pub fn inject_drop(id: &str, dest: &Path, names: &[String]) -> Result<(), AgentError> {
    harness_record_drop(id, names);
    #[cfg(windows)]
    {
        return crate::win_dnd::inject(id, dest, names);
    }
    #[cfg(target_os = "linux")]
    {
        return crate::x11_dnd::inject(id, dest, names);
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        tracing::info!(
            target: "ternvale::agent",
            id,
            dest = %dest.display(),
            count = names.len(),
            "dnd inject stub (no OLE/XDND on this host)"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_missing_id_is_ok() {
        cleanup("no-such-transfer").expect("missing");
    }
}
