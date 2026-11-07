//! Windows virtio-fs drive-letter mapping (WinFsp + virtio-win `virtiofs.exe`).
//!
//! `mount` is compiled only for Windows guests; helpers stay available so
//! host-side unit tests can check drive-letter parsing and the service command.

#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};

/// Default install location written by `Install-TernvaleTools.ps1`.
pub const VIRTIOFS_EXE: &str = r"C:\Program Files\Ternvale\virtiofs.exe";
/// virtio-win service name from the guest tools installer.
pub const SERVICE: &str = "VirtioFsSvc";

/// Normalize a guest mount point to `X:` for `virtiofs.exe -m`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::agent",
    skip_all,
    fields(mount_point)
)]
pub fn drive_letter(mount_point: &str) -> Option<String> {
    let trimmed = mount_point.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return None;
    }
    let rest = &trimmed[2..];
    if !(rest.is_empty()
        || rest == "\\"
        || rest == "/"
        || rest.starts_with('\\')
        || rest.starts_with('/'))
    {
        return None;
    }
    Some(format!("{}:", bytes[0].to_ascii_uppercase() as char))
}

/// `sc.exe` ImagePath: `virtiofs.exe -i -m X: -t TAG`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::agent",
    skip_all,
    fields(tag, mount)
)]
pub fn service_bin_path(exe: &Path, tag: &str, mount: &str) -> String {
    format!("{} -i -m {mount} -t {tag}", quote_win(exe))
}

fn quote_win(path: &Path) -> String {
    let text = path.display().to_string();
    if text.contains(' ') {
        format!("\"{text}\"")
    } else {
        text
    }
}

/// Candidate `virtiofs.exe` locations (tools ISO copy first).
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn virtiofs_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from(VIRTIOFS_EXE),
        PathBuf::from(r"C:\Program Files\viofs\virtiofs.exe"),
        PathBuf::from(r"C:\Program Files (x86)\WinFsp\bin\virtiofs.exe"),
    ]
}

/// Configure and start `VirtioFsSvc` so the share is a drive letter.
#[cfg(windows)]
#[tracing::instrument(
    level = "info",
    target = "ternvale::agent",
    skip_all,
    fields(tag, mount_point)
)]
pub fn mount(tag: &str, mount_point: &str) -> Result<(), crate::AgentError> {
    let drive = match drive_letter(mount_point) {
        Some(drive) => drive,
        None => {
            tracing::info!(
                target: "ternvale::agent",
                mount_point,
                "Windows share_mount is not a drive letter; mapping to Z:"
            );
            "Z:".to_string()
        }
    };
    let exe = virtiofs_candidates()
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| crate::AgentError::Action {
            action: "find virtiofs.exe",
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "virtiofs.exe missing; run Install-TernvaleTools.ps1",
            ),
        })?;
    let bin = service_bin_path(&exe, tag, &drive);
    tracing::info!(
        target: "ternvale::agent",
        exe = %exe.display(),
        bin = %bin,
        drive = %drive,
        tag,
        "mapping virtio-fs share to a Windows drive letter"
    );
    let _ = sc(&["stop", SERVICE]);
    let config = sc(&[
        "config",
        SERVICE,
        "binPath=",
        &bin,
        "start=",
        "delayed-auto",
    ])?;
    if !config.success() {
        let create = sc(&[
            "create",
            SERVICE,
            "binPath=",
            &bin,
            "start=",
            "delayed-auto",
            "depend=",
            "VirtioFsDrv",
            "DisplayName=",
            "VirtIO-FS",
        ])?;
        if !create.success() {
            return Err(crate::AgentError::Action {
                action: "sc config VirtioFsSvc",
                source: std::io::Error::from_raw_os_error(create.code().unwrap_or(-1)),
            });
        }
    }
    let start = sc(&["start", SERVICE])?;
    if !start.success() {
        tracing::warn!(
            target: "ternvale::agent",
            tag,
            drive = %drive,
            "VirtioFsSvc start failed"
        );
        return Err(crate::AgentError::Action {
            action: "sc start VirtioFsSvc",
            source: std::io::Error::from_raw_os_error(start.code().unwrap_or(-1)),
        });
    }
    tracing::info!(
        target: "ternvale::agent",
        tag,
        drive = %drive,
        "shared folder mapped at Windows logon"
    );
    Ok(())
}

#[cfg(windows)]
fn sc(args: &[&str]) -> Result<std::process::ExitStatus, crate::AgentError> {
    std::process::Command::new("sc.exe")
        .args(args)
        .status()
        .map_err(|source| crate::AgentError::Action {
            action: "sc.exe",
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::{drive_letter, service_bin_path};
    use std::path::Path;

    #[test]
    fn drive_letter_accepts_windows_mounts() {
        assert_eq!(drive_letter("Z:").as_deref(), Some("Z:"));
        assert_eq!(drive_letter("z:\\").as_deref(), Some("Z:"));
        assert_eq!(drive_letter("D:/share").as_deref(), Some("D:"));
        assert!(drive_letter("/mnt/ternvale").is_none());
        assert!(drive_letter("not-a-drive").is_none());
    }

    #[test]
    fn service_command_enables_case_insensitive_mount() {
        let cmd = service_bin_path(
            Path::new(r"C:\Program Files\Ternvale\virtiofs.exe"),
            "ternvale",
            "Z:",
        );
        assert!(cmd.contains("-i"), "{cmd}");
        assert!(cmd.contains("-m Z:"), "{cmd}");
        assert!(cmd.contains("-t ternvale"), "{cmd}");
        assert!(cmd.contains('\"'), "{cmd}");
    }
}
