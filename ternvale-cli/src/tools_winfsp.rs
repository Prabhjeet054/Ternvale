//! Stage the unmodified WinFsp MSI onto the tools ISO.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

const FETCH_SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/fetch-winfsp.sh");

/// Locate or fetch `winfsp.msi` (never required for unit-test ISO builds).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn resolve(explicit: Option<&Path>, fetch: bool) -> Result<Option<PathBuf>> {
    if let Some(path) = explicit {
        return require_msi(path).map(Some);
    }
    if let Some(path) = std::env::var_os("TERNVALE_WINFSP_MSI") {
        return require_msi(Path::new(&path)).map(Some);
    }
    let cache = default_msi_path()?;
    if cache.is_file() {
        return Ok(Some(cache));
    }
    if fetch {
        return fetch_msi().map(Some);
    }
    tracing::info!(
        target: "ternvale::cli",
        "no WinFsp MSI; tools ISO will omit winfsp/ (see docs/LICENSES.md)"
    );
    Ok(None)
}

/// Copy the MSI to `dest/winfsp.msi`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(src = %src.display()))]
pub fn stage(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).context("create winfsp staging dir")?;
    let out = dest.join("winfsp.msi");
    fs::copy(src, &out).with_context(|| format!("copy {} -> {}", src.display(), out.display()))?;
    tracing::info!(
        target: "ternvale::cli",
        dest = %out.display(),
        "staged unmodified WinFsp MSI"
    );
    Ok(())
}

fn default_msi_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/Caches/Ternvale/winfsp/winfsp.msi"))
}

fn fetch_msi() -> Result<PathBuf> {
    let dest = default_msi_path()?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("winfsp cache path has no parent"))?;
    fs::create_dir_all(&dest).with_context(|| format!("create {}", dest.display()))?;
    let script = Path::new(FETCH_SCRIPT);
    if !script.is_file() {
        bail!("fetch script missing: {}", script.display());
    }
    tracing::info!(
        target: "ternvale::cli",
        dest = %dest.display(),
        "fetching WinFsp MSI (see docs/LICENSES.md)"
    );
    let status = Command::new("bash")
        .arg(script)
        .arg(&dest)
        .status()
        .context("spawn scripts/fetch-winfsp.sh")?;
    if !status.success() {
        bail!("scripts/fetch-winfsp.sh failed ({status})");
    }
    require_msi(&dest.join("winfsp.msi"))
}

fn require_msi(path: &Path) -> Result<PathBuf> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    Err(anyhow!("WinFsp MSI is not a file: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::require_msi;
    use std::path::Path;

    #[test]
    fn missing_msi_is_an_error() {
        let err = require_msi(Path::new("/no/such/winfsp.msi")).expect_err("missing");
        assert!(err.to_string().contains("not a file"), "{err}");
    }
}
