//! Concurrent macOS guest cap enforced by Virtualization.framework.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use ternvale_config::{Profile, VmConfig};

use crate::BackendError;

/// Apple's kernel quota (`hv_apple_isa_vm_quota`) and SLA §2B(iii): two
/// additional macOS instances. `VZError.Code.virtualMachineLimitExceeded` (6).
/// TODO(verify) if a later macOS raises this.
pub const MACOS_CONCURRENT_LIMIT: u32 = 2;

/// Names of currently running Ternvale macOS guests, excluding `skip`.
#[tracing::instrument(level = "debug", target = "ternvale::macos", skip_all, fields(skip))]
pub fn running_macos_guests(skip: Option<&str>) -> Result<Vec<String>, BackendError> {
    let run = run_dir();
    let vms = vms_dir();
    let mut names = Vec::new();
    let entries = match std::fs::read_dir(&run) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(names),
        Err(error) => {
            return Err(BackendError::Io {
                op: "macos_guest_limit",
                detail: format!("read {}: {error}", run.display()),
            })
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| BackendError::Io {
            op: "macos_guest_limit",
            detail: error.to_string(),
        })?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sock") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if name == "api" {
            continue;
        }
        if skip.is_some_and(|s| s == name) {
            continue;
        }
        if !socket_alive(&path) {
            continue;
        }
        let config_path = vms.join(name).join("config.toml");
        if !config_path.is_file() {
            continue;
        }
        match VmConfig::from_path(&config_path) {
            Ok(config) if config.profile == Profile::Macos => names.push(name.to_string()),
            Ok(_) => {}
            Err(error) => tracing::warn!(
                target: "ternvale::macos",
                path = %config_path.display(),
                error = %error,
                "skipping vm while counting macos guests"
            ),
        }
    }
    names.sort();
    tracing::debug!(
        target: "ternvale::macos",
        count = names.len(),
        skip,
        "counted running macos guests"
    );
    Ok(names)
}

/// Fail before `VZVirtualMachine.start` if two macOS guests are already up.
#[tracing::instrument(level = "info", target = "ternvale::macos", skip_all, fields(vm))]
pub fn refuse_if_at_limit(vm: &str) -> Result<(), BackendError> {
    let running = running_macos_guests(Some(vm))?;
    if running.len() as u32 >= MACOS_CONCURRENT_LIMIT {
        let list = running.join(", ");
        tracing::warn!(
            target: "ternvale::macos",
            vm,
            running = %list,
            limit = MACOS_CONCURRENT_LIMIT,
            "refusing macos start; concurrent limit"
        );
        return Err(BackendError::Io {
            op: "macos_guest_limit",
            detail: format!(
                "cannot start macOS guest {vm}: {MACOS_CONCURRENT_LIMIT} macOS guests are already running ({list}). Apple's Virtualization.framework allows two concurrent macOS VMs (VZErrorVirtualMachineLimitExceeded; macOS SLA §2B(iii)). Stop one first."
            ),
        });
    }
    Ok(())
}

fn socket_alive(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

fn run_dir() -> PathBuf {
    std::env::var_os("TERNVALE_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("Library/Application Support/Ternvale/run"))
}

fn vms_dir() -> PathBuf {
    std::env::var_os("TERNVALE_VMS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("Library/Application Support/Ternvale/vms"))
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_run_dir_is_zero() {
        let dir = std::env::temp_dir().join(format!("tv-macos-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        // SAFETY: this test owns the env keys; workspace tests use --test-threads=1.
        unsafe {
            std::env::set_var("TERNVALE_RUN_DIR", &dir);
            std::env::set_var("TERNVALE_VMS_DIR", dir.join("vms"));
        }
        let names = running_macos_guests(None).expect("count");
        assert!(names.is_empty(), "{names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuse_message_names_limit() {
        let err = BackendError::Io {
            op: "macos_guest_limit",
            detail: format!(
                "cannot start macOS guest x: {MACOS_CONCURRENT_LIMIT} macOS guests are already running (a, b)."
            ),
        };
        let text = err.to_string();
        assert!(text.contains("macos_guest_limit"), "{text}");
        assert!(text.contains("2 macOS guests"), "{text}");
    }
}
