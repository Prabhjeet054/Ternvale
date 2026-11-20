//! Windows OLE drop target / drag source (live path gated).
//!
//! OLE `IDropTarget::Drop` and `DoDragDrop` (MSDN: IDropTarget, IDropSource,
//! CF_HDROP, CF_DIB, RegisterClipboardFormat("Rich Text Format"), CF_HTML).
//! Live injection on Windows 11 ARM64 is TODO(verify).

use std::path::Path;

use crate::AgentError;

/// Inject staged files into the foreground window via OLE.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id))]
#[cfg_attr(not(windows), allow(dead_code))]
pub fn inject(id: &str, dest: &Path, names: &[String]) -> Result<(), AgentError> {
    tracing::warn!(
        target: "ternvale::agent",
        id,
        dest = %dest.display(),
        count = names.len(),
        "TODO(verify) OLE IDropTarget::Drop on Windows 11 ARM64; files stay in the stage dir"
    );
    let _ = (id, dest, names);
    Ok(())
}

/// Begin a host-bound drag (`DoDragDrop` with promised files).
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id))]
#[allow(dead_code)]
pub fn begin_outbound(id: &str, names: &[String]) -> Result<(), AgentError> {
    tracing::warn!(
        target: "ternvale::agent",
        id,
        count = names.len(),
        "TODO(verify) DoDragDrop promised files on Windows 11 ARM64"
    );
    Ok(())
}
