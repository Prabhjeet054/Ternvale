//! Build an ISO 9660 + Joliet image with `hdiutil` or `xorriso`.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Create `out` from the staging directory using the first available tool.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(staging = %staging.display(), out = %out.display())
)]
pub fn make_hybrid_iso(staging: &Path, out: &Path) -> Result<()> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create ISO parent {}", parent.display()))?;
    }
    if which("hdiutil") {
        hdiutil_makehybrid(staging, out)?;
        return settle_hdiutil_output(out);
    }
    if which("xorriso") {
        return xorriso_make(staging, out);
    }
    Err(anyhow!(
        "neither hdiutil nor xorriso is on PATH; install Xcode CLT or xorriso to build ternvale-tools.iso"
    ))
}

fn which(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
fn hdiutil_makehybrid(staging: &Path, out: &Path) -> Result<()> {
    tracing::info!(
        target: "ternvale::cli",
        staging = %staging.display(),
        out = %out.display(),
        "building ISO with hdiutil makehybrid"
    );
    let output = Command::new("hdiutil")
        .args([
            "makehybrid",
            "-iso",
            "-joliet",
            "-default-volume-name",
            "TERNVALE",
            "-o",
        ])
        .arg(out)
        .arg(staging)
        .output()
        .context("spawn hdiutil makehybrid")?;
    if !output.status.success() {
        return Err(anyhow!(
            "hdiutil makehybrid failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

/// `hdiutil makehybrid -o foo.iso` sometimes writes `foo.iso.cdr`.
fn settle_hdiutil_output(out: &Path) -> Result<()> {
    if out.is_file() {
        return Ok(());
    }
    let cdr = PathBuf::from(format!("{}.cdr", out.display()));
    if cdr.is_file() {
        std::fs::rename(&cdr, out)
            .with_context(|| format!("rename {} -> {}", cdr.display(), out.display()))?;
        return Ok(());
    }
    Err(anyhow!(
        "hdiutil makehybrid did not produce {}",
        out.display()
    ))
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
fn xorriso_make(staging: &Path, out: &Path) -> Result<()> {
    tracing::info!(
        target: "ternvale::cli",
        staging = %staging.display(),
        out = %out.display(),
        "building ISO with xorriso"
    );
    let output = Command::new("xorriso")
        .args(["-as", "mkisofs", "-R", "-J", "-V", "TERNVALE", "-o"])
        .arg(out)
        .arg(staging)
        .output()
        .context("spawn xorriso")?;
    if !output.status.success() {
        return Err(anyhow!(
            "xorriso failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
