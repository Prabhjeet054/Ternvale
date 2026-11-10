//! Optional `qemu-img` for qcow2/vmdk/vhdx/raw import.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Disk formats `qemu-img convert` can read for Ternvale import.
pub const FOREIGN_EXTS: &[&str] = &["qcow2", "vmdk", "vhdx", "raw", "img"];

/// Locate `qemu-img` (`PATH`, then Homebrew prefixes).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn find() -> Result<PathBuf, String> {
    let candidates = [
        PathBuf::from("qemu-img"),
        PathBuf::from("/opt/homebrew/bin/qemu-img"),
        PathBuf::from("/usr/local/bin/qemu-img"),
    ];
    for bin in candidates {
        match Command::new(&bin).arg("-V").output() {
            Ok(out) if out.status.success() => {
                let path = if bin.components().count() == 1 {
                    which("qemu-img").unwrap_or(bin)
                } else {
                    bin
                };
                tracing::debug!(
                    target: "ternvale::cli",
                    path = %path.display(),
                    "qemu-img found"
                );
                return Ok(path);
            }
            Ok(_) | Err(_) => continue,
        }
    }
    tracing::debug!(target: "ternvale::cli", "qemu-img not found");
    Err("qemu-img not found (optional; needed to import qcow2/vmdk/vhdx)".into())
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// True when `path` looks like a foreign disk image, not a `.tvm` directory.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn is_foreign_disk(path: &Path) -> bool {
    if path.is_dir() {
        return false;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    FOREIGN_EXTS.iter().any(|want| *want == ext)
}

/// `qemu-img convert -O raw` into `dest` (must not exist).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(src = %src.display(), dest = %dest.display())
)]
pub fn convert_to_raw(src: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        bail!("refusing to overwrite {}", dest.display());
    }
    let bin = find().map_err(|error| anyhow::anyhow!("{error}"))?;
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("raw")
        .to_ascii_lowercase();
    let fmt = match ext.as_str() {
        "img" => "raw",
        other => other,
    };
    tracing::info!(
        target: "ternvale::cli",
        bin = %bin.display(),
        fmt,
        "qemu-img convert"
    );
    let out = Command::new(&bin)
        .args(["convert", "-p", "-f", fmt, "-O", "raw"])
        .arg(src)
        .arg(dest)
        .output()
        .with_context(|| format!("run {} convert", bin.display()))?;
    if !out.status.success() {
        tracing::error!(
            target: "ternvale::cli",
            status = ?out.status.code(),
            "qemu-img convert failed"
        );
        bail!(
            "qemu-img convert failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_exts_and_directories() {
        assert!(is_foreign_disk(Path::new("disk.qcow2")));
        assert!(is_foreign_disk(Path::new("x.vmdk")));
        assert!(is_foreign_disk(Path::new("x.vhdx")));
        assert!(is_foreign_disk(Path::new("x.raw")));
        assert!(!is_foreign_disk(Path::new("vm.tvm")));
        let dir = std::env::temp_dir().join(format!("tvm-q-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert!(!is_foreign_disk(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
