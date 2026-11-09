//! Apply the Seatbelt profile and re-exec the VMM as a child.

use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use ternvale_config::VmConfig;
use ternvale_hv::ScopedAccess;

use super::bookmarks;
use super::env::{skip_sandbox, SANDBOXED_ENV};
use super::error::SandboxError;
use super::grants::Grants;
use super::profile::{self, SANDBOX_EXEC};

/// Env pointing at the bookmark store for the child.
pub const BOOKMARKS_ENV: &str = "TERNVALE_BOOKMARKS";

/// `Some(code)` when this process launched a confined child and waited for it.
/// `None` when this process should boot the VM itself.
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all)]
pub fn maybe_relaunch(
    config_path: &Path,
    exe: &Path,
    args: &[String],
    config: &VmConfig,
) -> Result<Option<ExitCode>, SandboxError> {
    if super::env::is_sandboxed() {
        tracing::info!(target: "ternvale::sandbox", "already inside Seatbelt; booting the VM");
        return Ok(None);
    }
    if skip_sandbox() {
        return Ok(None);
    }
    if !Path::new(SANDBOX_EXEC).is_file() {
        return Err(SandboxError::NoSandboxExec(
            Path::new(SANDBOX_EXEC).to_path_buf(),
        ));
    }
    let grants = Grants::from_config(config, config_path);
    let profile_text = profile::render(&grants);
    let dir = std::env::temp_dir().join(format!(
        "ternvale-sandbox-{}-{}",
        std::process::id(),
        config.name
    ));
    std::fs::create_dir_all(&dir).map_err(|source| SandboxError::Io {
        op: "create seatbelt work dir",
        source,
    })?;
    let profile_path = dir.join("vmm.sb");
    std::fs::write(&profile_path, profile_text).map_err(|source| SandboxError::Io {
        op: "write seatbelt profile",
        source,
    })?;
    let bookmark_path = dir.join("bookmarks.tvbk");
    bookmarks::write_store(&bookmark_path, &grants.paths)?;
    tracing::info!(
        target: "ternvale::sandbox",
        profile = %profile_path.display(),
        bookmarks = %bookmark_path.display(),
        child = %exe.display(),
        "launching VMM under Seatbelt"
    );
    let mut cmd = Command::new(SANDBOX_EXEC);
    cmd.arg("-f")
        .arg(&profile_path)
        .arg(exe)
        .args(args)
        .env(SANDBOXED_ENV, "1")
        .env(BOOKMARKS_ENV, &bookmark_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = cmd
        .status()
        .map_err(|source| SandboxError::Spawn(source.to_string()))?;
    let _ = std::fs::remove_dir_all(&dir);
    let code = status
        .code()
        .map(|c| ExitCode::from(c as u8))
        .unwrap_or(ExitCode::FAILURE);
    Ok(Some(code))
}

/// Start accessing bookmarks listed in [`BOOKMARKS_ENV`], if any.
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all)]
pub fn hold_bookmarks() -> Result<Vec<ScopedAccess>, SandboxError> {
    let Ok(path) = std::env::var(BOOKMARKS_ENV) else {
        return Ok(Vec::new());
    };
    bookmarks::start_access(Path::new(&path))
}

#[cfg(test)]
mod tests {
    use super::maybe_relaunch;
    use crate::env::SANDBOXED_ENV;
    use ternvale_config::VmConfig;

    #[test]
    fn sandboxed_process_does_not_relaunch() {
        if std::env::var(SANDBOXED_ENV).ok().as_deref() != Some("1") {
            return;
        }
        let config = VmConfig::parse(
            "name = \"p\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n",
        )
        .unwrap();
        let out = maybe_relaunch(
            std::path::Path::new("/tmp/vm.toml"),
            std::path::Path::new("/bin/true"),
            &[],
            &config,
        )
        .expect("relaunch");
        assert!(out.is_none());
    }
}
