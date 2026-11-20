//! Drag-and-drop offer validation (host Finder ↔ guest OLE/XDND).

use crate::{validate_reason, MAX_COPY_PATH, MAX_NAME};

/// Max files in one drag offer.
pub const MAX_DRAG_FILES: usize = 4096;
/// Longest drag correlation id (decimal or uuid-like).
pub const MAX_DRAG_ID: usize = 64;

/// Range checks for `drag_offer`.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(id))]
pub fn validate_drag_offer(id: &str, names: &[String], sizes: &[u64]) -> Result<(), String> {
    validate_drag_id(id)?;
    if names.is_empty() || names.len() > MAX_DRAG_FILES {
        return Err(format!(
            "drag_offer names length must be 1..={MAX_DRAG_FILES}"
        ));
    }
    if !sizes.is_empty() && sizes.len() != names.len() {
        return Err("drag_offer sizes must match names".into());
    }
    for name in names {
        if name.is_empty() || name.len() > MAX_COPY_PATH || name.contains('\0') {
            return Err("drag_offer name empty, too long, or contains NUL".into());
        }
    }
    Ok(())
}

/// Drag correlation id.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn validate_drag_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > MAX_DRAG_ID || id.contains('\0') {
        Err(format!("drag id length must be 1..={MAX_DRAG_ID}"))
    } else {
        Ok(())
    }
}

/// `drag_accept` dest is a hint (absolute guest/host path or empty for default).
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn validate_drag_accept(id: &str, dest: &str) -> Result<(), String> {
    validate_drag_id(id)?;
    if dest.len() > MAX_COPY_PATH || dest.contains('\0') {
        Err("drag_accept dest too long or contains NUL".into())
    } else {
        Ok(())
    }
}

/// `drag_cancel` reason.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn validate_drag_cancel(id: &str, reason: &str) -> Result<(), String> {
    validate_drag_id(id)?;
    validate_reason(reason)
}

/// `drag_progress` totals.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn validate_drag_progress(id: &str, done: u64, total: u64) -> Result<(), String> {
    validate_drag_id(id)?;
    if done > total {
        Err("drag_progress done exceeds total".into())
    } else if total > 0 && id.len() > MAX_NAME {
        Err("drag id too long".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_requires_names() {
        assert!(validate_drag_offer("1", &[], &[]).is_err());
        assert!(validate_drag_offer("1", &["a.txt".into()], &[4]).is_ok());
        assert!(validate_drag_offer("", &["a.txt".into()], &[]).is_err());
    }
}
