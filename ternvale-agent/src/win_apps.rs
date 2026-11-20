//! Windows Start Menu `.lnk` walk, packaged-app stub, and `ShellExecuteW`.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use ternvale_agent_proto::{AppSource, GuestApp, MAX_APPS};

use crate::AgentError;

/// Collect `%ProgramData%` and `%AppData%` Start Menu `.lnk` files.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn collect_start_menu(apps: &mut Vec<GuestApp>) {
    #[cfg(windows)]
    {
        if let Some(root) = std::env::var_os("ProgramData") {
            walk(
                std::path::Path::new(&root).join("Microsoft/Windows/Start Menu/Programs"),
                apps,
            );
        }
        if let Some(root) = std::env::var_os("AppData") {
            walk(
                std::path::Path::new(&root).join("Microsoft/Windows/Start Menu/Programs"),
                apps,
            );
        }
        tracing::debug!(target: "ternvale::agent", count = apps.len(), "Start Menu .lnk walk");
    }
    #[cfg(not(windows))]
    {
        let _ = apps;
    }
}

/// Packaged / UWP apps (`shell:AppsFolder`).
///
/// TODO(verify): enumerate AUMIDs via `IApplicationActivationManager` /
/// `shell:AppsFolder` on Windows 11 ARM64 without a full COM shell dump.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn collect_packaged(apps: &mut Vec<GuestApp>) {
    let _ = apps;
    tracing::debug!(
        target: "ternvale::agent",
        "packaged app enumeration is TODO(verify); Start Menu .lnk list is used"
    );
}

/// Launch by Start-menu relative id or `shell:AppsFolder\\AUMID`.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id))]
pub fn launch(id: &str, file: Option<&str>) -> Result<(), AgentError> {
    #[cfg(windows)]
    {
        let path = resolve_id(id).ok_or_else(|| AgentError::Action {
            action: "app launch",
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "unknown app id"),
        })?;
        return shell_execute(&path, file);
    }
    #[cfg(not(windows))]
    {
        let _ = (id, file);
        Err(AgentError::Action {
            action: "app launch",
            source: std::io::Error::new(std::io::ErrorKind::Unsupported, "not Windows"),
        })
    }
}

#[cfg(windows)]
fn resolve_id(id: &str) -> Option<std::path::PathBuf> {
    if id.starts_with("shell:") {
        return Some(std::path::PathBuf::from(id));
    }
    let rel = std::path::Path::new(id);
    for root_var in ["ProgramData", "AppData"] {
        if let Some(root) = std::env::var_os(root_var) {
            let candidate = std::path::Path::new(&root)
                .join("Microsoft/Windows/Start Menu/Programs")
                .join(rel);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(windows)]
fn shell_execute(path: &std::path::Path, file: Option<&str>) -> Result<(), AgentError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::HINSTANCE;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }
    let file_w = file.map(|f| wide(std::ffi::OsStr::new(f)));
    let path_w = wide(path.as_os_str());
    let verb = wide(std::ffi::OsStr::new("open"));
    let params = file_w
        .as_deref()
        .map(|w| w.as_ptr())
        .unwrap_or(std::ptr::null());
    // SAFETY: LPCWSTR buffers are NUL-terminated and live for the call.
    // TODO(verify): ShellExecuteW on a `.lnk` plus a share-mapped file on Win11 ARM64.
    let rc: HINSTANCE = unsafe {
        ShellExecuteW(
            0,
            verb.as_ptr(),
            path_w.as_ptr(),
            params,
            std::ptr::null(),
            SW_SHOWNORMAL as i32,
        )
    };
    if (rc as usize) <= 32 {
        tracing::warn!(target: "ternvale::agent", rc = rc as usize, path = %path.display(), "ShellExecuteW failed");
        return Err(AgentError::Action {
            action: "ShellExecuteW",
            source: std::io::Error::from_raw_os_error(rc as i32),
        });
    }
    tracing::info!(target: "ternvale::agent", path = %path.display(), "ShellExecuteW launched app");
    Ok(())
}

#[cfg(windows)]
fn walk(root: std::path::PathBuf, apps: &mut Vec<GuestApp>) {
    walk_inner(&root, &root, apps);
}

#[cfg(windows)]
fn walk_inner(root: &std::path::Path, dir: &std::path::Path, apps: &mut Vec<GuestApp>) {
    if apps.len() >= MAX_APPS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if apps.len() >= MAX_APPS {
            break;
        }
        let path = entry.path();
        if path.is_dir() {
            walk_inner(root, &path, apps);
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.to_ascii_lowercase().ends_with(".lnk") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let id = rel.to_string_lossy().replace('/', "\\");
        let display = name
            .strip_suffix(".lnk")
            .or_else(|| name.strip_suffix(".LNK"))
            .unwrap_or(name);
        apps.push(GuestApp {
            id,
            name: display.to_string(),
            exe: path.to_string_lossy().into_owned(),
            source: AppSource::StartMenu,
            extensions: Vec::new(),
            icon: extract_icon(&path),
        });
    }
}

/// TODO(verify): `ExtractIconExW` → PNG on Windows 11 ARM64; omitted for now
/// so we never send unbounded icon bytes.
#[cfg(windows)]
fn extract_icon(_path: &std::path::Path) -> Option<ternvale_agent_proto::HexBytes> {
    None
}

#[cfg(not(windows))]
fn extract_icon(_path: &std::path::Path) -> Option<ternvale_agent_proto::HexBytes> {
    None
}
