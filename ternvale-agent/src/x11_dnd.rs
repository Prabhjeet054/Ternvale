//! Linux XDND drop / drag (live path gated).
//!
//! XDND protocol (freedesktop.org/wiki/Specifications/XDND): `XdndEnter`,
//! `XdndPosition`, `XdndDrop`, `XdndFinished`, `text/uri-list`. Live path
//! against mutter/kwin is TODO(verify).

use std::path::Path;

use crate::AgentError;

/// Drop staged files onto the focused X11/Wayland client.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn inject(id: &str, dest: &Path, names: &[String]) -> Result<(), AgentError> {
    tracing::info!(
        target: "ternvale::agent",
        id,
        dest = %dest.display(),
        count = names.len(),
        "TODO(verify) XDND inject; files remain in the stage directory"
    );
    let _ = (id, dest, names);
    Ok(())
}

/// Start an XDND drag from the guest toward the host.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id))]
#[allow(dead_code)]
pub fn begin_outbound(id: &str, names: &[String]) -> Result<(), AgentError> {
    tracing::warn!(
        target: "ternvale::agent",
        id,
        count = names.len(),
        "TODO(verify) XDND source toward the host compositor"
    );
    Ok(())
}
