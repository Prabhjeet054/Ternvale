//! Debug-only host accesses the Seatbelt profile must deny.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Env that forces the probe even in release builds.
pub const PROBE_ENV: &str = "TERNVALE_SANDBOX_PROBE";
/// Set on the self-test child so it only runs the probe and exits.
pub const PROBE_CHILD_ENV: &str = "TERNVALE_SANDBOX_PROBE_CHILD";

/// One attempted access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeItem {
    /// What was attempted.
    pub name: &'static str,
    /// Path or binary.
    pub target: String,
    /// True when the kernel/Seatbelt blocked it.
    pub denied: bool,
    /// OS error or unexpected success text (never secret file contents).
    pub detail: String,
}

/// Outcome of `~/.ssh`, `/etc/hosts`, and `/bin/sh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    /// Items in a fixed order.
    pub items: [ProbeItem; 3],
}

impl ProbeReport {
    /// True when every attempt was denied.
    #[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all)]
    pub fn all_denied(&self) -> bool {
        self.items.iter().all(|item| item.denied)
    }
}

/// Try to read `~/.ssh` and `/etc/hosts` and to spawn `/bin/sh`.
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all)]
pub fn run_debug_probe() -> ProbeReport {
    let ssh_dir = home_ssh();
    let ssh = probe_read("ssh", &ssh_dir);
    // macOS `/etc` is a symlink onto `/private/etc`; probe both so a profile
    // that only denies the public path cannot look like it is working.
    let hosts_pub = probe_read("hosts", PathBuf::from("/etc/hosts").as_path());
    let hosts_priv = probe_read("hosts", PathBuf::from("/private/etc/hosts").as_path());
    let hosts = ProbeItem {
        name: "hosts",
        target: "/etc/hosts|/private/etc/hosts".into(),
        denied: hosts_pub.denied && hosts_priv.denied,
        detail: format!("etc={}; private={}", hosts_pub.detail, hosts_priv.detail),
    };
    let sh = probe_spawn();
    let report = ProbeReport {
        items: [ssh, hosts, sh],
    };
    for item in &report.items {
        if item.denied {
            tracing::info!(
                target: "ternvale::sandbox",
                name = item.name,
                target = %item.target,
                detail = %item.detail,
                "sandbox probe denied"
            );
        } else {
            tracing::error!(
                target: "ternvale::sandbox",
                name = item.name,
                target = %item.target,
                detail = %item.detail,
                "sandbox probe was allowed; confinement is too weak"
            );
        }
    }
    report
}

/// True when the operator asked for the probe (`TERNVALE_SANDBOX_PROBE=1`).
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all)]
pub fn probe_requested() -> bool {
    std::env::var(PROBE_ENV).is_ok_and(|v| v == "1")
}

fn home_ssh() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_else(|| "/Users/unknown".into());
    PathBuf::from(home).join(".ssh")
}

fn probe_read(name: &'static str, path: &std::path::Path) -> ProbeItem {
    let target = path.display().to_string();
    match fs::read(path) {
        Ok(_) => ProbeItem {
            name,
            target,
            denied: false,
            detail: "read succeeded".into(),
        },
        Err(error) if is_dir_err(&error) => match fs::read_dir(path) {
            Ok(_) => ProbeItem {
                name,
                target,
                denied: false,
                detail: "read_dir succeeded".into(),
            },
            Err(dir_err) => ProbeItem {
                name,
                target,
                denied: is_denied(&dir_err),
                detail: dir_err.kind().to_string(),
            },
        },
        Err(error) => ProbeItem {
            name,
            target,
            denied: is_denied(&error),
            detail: error.kind().to_string(),
        },
    }
}

fn is_dir_err(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(21) // EISDIR
        || error.kind() == std::io::ErrorKind::IsADirectory
}

fn probe_spawn() -> ProbeItem {
    match Command::new("/bin/sh")
        .args(["-c", "echo ternvale-sandbox-pwned"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => ProbeItem {
            name: "sh",
            target: "/bin/sh".into(),
            denied: false,
            detail: "exit 0".into(),
        },
        Ok(status) => ProbeItem {
            name: "sh",
            target: "/bin/sh".into(),
            denied: true,
            detail: format!("exit {status}"),
        },
        Err(error) => ProbeItem {
            name: "sh",
            target: "/bin/sh".into(),
            denied: is_denied(&error),
            detail: error.kind().to_string(),
        },
    }
}

fn is_denied(error: &std::io::Error) -> bool {
    // Any failure to read or exec is confinement from the probe's point of
    // view (EPERM, ENOENT, EISDIR after a denied listing, uncategorized).
    error.raw_os_error() == Some(1) // EPERM
        || error.raw_os_error() == Some(13) // EACCES
        || matches!(
            error.kind(),
            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
        )
}

#[cfg(test)]
mod tests {
    use super::{probe_read, ProbeItem};
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn missing_path_is_treated_as_denied() {
        let item = probe_read("ssh", Path::new("/no/such/ternvale-ssh-dir"));
        assert!(item.denied, "{item:?}");
        assert_eq!(item.name, "ssh");
    }

    #[test]
    fn probe_item_debug_has_no_file_bytes() {
        let item = ProbeItem {
            name: "hosts",
            target: "/etc/hosts".into(),
            denied: true,
            detail: "permission denied".into(),
        };
        let text = format!("{item:?}");
        assert!(!text.contains("localhost"));
    }

    #[test]
    fn seatbelt_denies_hosts_ssh_and_sh() {
        if !Path::new(crate::SANDBOX_EXEC).is_file() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("tv-probe-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let profile = dir.join("probe.sb");
        let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/unknown".into());
        let ssh = format!("{home}/.ssh");
        std::fs::write(
            &profile,
            "(version 1)\n(deny default)\n(deny process-exec)\n(deny process-fork)\n\
             (allow file-read-metadata)\n(allow sysctl-read)\n(allow mach-lookup)\n\
             (allow file-read* (subpath \"/usr\") (subpath \"/System\") (subpath \"/bin\"))\n",
        )
        .expect("profile");
        let hosts = Command::new(crate::SANDBOX_EXEC)
            .args(["-f", &profile.to_string_lossy(), "/bin/cat", "/etc/hosts"])
            .output()
            .expect("cat hosts");
        assert!(
            !hosts.status.success(),
            "cat /etc/hosts should be denied: stdout={} stderr={}",
            String::from_utf8_lossy(&hosts.stdout),
            String::from_utf8_lossy(&hosts.stderr)
        );
        let ssh_out = Command::new(crate::SANDBOX_EXEC)
            .args(["-f", &profile.to_string_lossy(), "/bin/ls", &ssh])
            .output()
            .expect("ls ssh");
        assert!(
            !ssh_out.status.success(),
            "ls ~/.ssh should be denied: stderr={}",
            String::from_utf8_lossy(&ssh_out.stderr)
        );
        let sh = Command::new(crate::SANDBOX_EXEC)
            .args([
                "-f",
                &profile.to_string_lossy(),
                "/bin/sh",
                "-c",
                "/bin/echo ternvale-sandbox-pwned",
            ])
            .output()
            .expect("sh");
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&sh.stdout),
            String::from_utf8_lossy(&sh.stderr)
        );
        assert!(
            !sh.status.success() || !combined.contains("ternvale-sandbox-pwned"),
            "spawn /bin/sh should be denied: {combined}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
