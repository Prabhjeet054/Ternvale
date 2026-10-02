//! Control socket location: `~/Library/Application Support/Ternvale/run/<name>.sock`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// `sockaddr_un.sun_path` is 104 bytes on macOS, including the NUL.
pub const MAX_SOCKET_PATH: usize = 103;

/// Overrides the run directory (tests, or a home path too long for a socket).
pub const RUN_DIR_ENV: &str = "TERNVALE_RUN_DIR";

/// The directory holding every VM's control socket.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn run_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(RUN_DIR_ENV) {
        tracing::debug!(target: "ternvale::cli", dir = ?dir, "run dir from {RUN_DIR_ENV}");
        return Ok(PathBuf::from(dir));
    }
    let home =
        std::env::var_os("HOME").context("HOME is not set; cannot find the run directory")?;
    Ok(PathBuf::from(home).join("Library/Application Support/Ternvale/run"))
}

/// Control socket path for VM `name` in [`run_dir`].
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name))]
pub fn socket_path(name: &str) -> Result<PathBuf> {
    socket_path_in(&run_dir()?, name)
}

/// Control socket path for VM `name` in `dir`. The name must be a plain
/// file-name token, and the result must fit in `sun_path`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name))]
pub fn socket_path_in(dir: &Path, name: &str) -> Result<PathBuf> {
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if !plain {
        tracing::warn!(target: "ternvale::cli", vm = name, "rejected vm name for a socket path");
        bail!("vm name {name:?} must be non-empty ASCII letters, digits, '-' or '_'");
    }
    let path = dir.join(format!("{name}.sock"));
    let len = path.as_os_str().len();
    if len > MAX_SOCKET_PATH {
        tracing::warn!(target: "ternvale::cli", path = %path.display(), len, "control socket path too long");
        bail!(
            "control socket path {} is {len} bytes; macOS allows {MAX_SOCKET_PATH} (set {RUN_DIR_ENV} to a shorter directory)",
            path.display()
        );
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_lives_in_the_run_dir_under_the_vm_name() {
        let path = socket_path_in(Path::new("/tmp/tv"), "vm-1_a").expect("path");
        assert_eq!(path, Path::new("/tmp/tv/vm-1_a.sock"));
    }

    #[test]
    fn rejects_names_that_escape_the_directory_or_overflow_sun_path() {
        for name in ["", "../x", "a/b", "a b", "ä"] {
            assert!(socket_path_in(Path::new("/tmp"), name).is_err(), "{name:?}");
        }
        let long = "x".repeat(MAX_SOCKET_PATH);
        let error = socket_path_in(Path::new("/tmp"), &long).expect_err("too long");
        assert!(format!("{error:#}").contains(RUN_DIR_ENV), "{error:#}");
    }

    #[test]
    fn default_run_dir_is_under_application_support() {
        if std::env::var_os(RUN_DIR_ENV).is_none() {
            let dir = run_dir().expect("run dir");
            assert!(
                dir.ends_with("Library/Application Support/Ternvale/run"),
                "{dir:?}"
            );
        }
    }
}
