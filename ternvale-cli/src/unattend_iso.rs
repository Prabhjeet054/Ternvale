//! Build a small ISO whose root contains `Autounattend.xml` and ARM64 drivers.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use ternvale_config::disc_format;

use crate::tools_hybrid::make_hybrid_iso;
use crate::unattend_xml::{self, UnattendSpec};

/// Stage answer file + viostor/NetKVM and wrap as ISO 9660.
#[tracing::instrument(level = "info", target = "ternvale::cli", skip_all, fields(out = %out.display(), tools = %tools.display()))]
pub fn build_unattend_iso(out: &Path, tools: &Path, spec: &UnattendSpec<'_>) -> Result<()> {
    require_tools_iso(tools)?;
    let staging = unique_dir("ternvale-unattend")?;
    let result = (|| {
        fs::create_dir_all(&staging).context("create unattend staging dir")?;
        let xml = unattend_xml::render(spec);
        fs::write(staging.join("Autounattend.xml"), xml.as_bytes())
            .context("write Autounattend.xml")?;
        stage_arm64_drivers(tools, &staging.join("drivers"))?;
        if out.exists() {
            fs::remove_file(out).with_context(|| format!("remove {}", out.display()))?;
        }
        make_hybrid_iso(&staging, out)?;
        tracing::info!(
            target: "ternvale::cli",
            out = %out.display(),
            "unattend ISO ready"
        );
        Ok(())
    })();
    if let Err(error) = fs::remove_dir_all(&staging) {
        tracing::warn!(
            target: "ternvale::cli",
            staging = %staging.display(),
            error = %error,
            "could not remove unattend staging directory"
        );
    }
    result
}

/// Tools ISO must exist, look like a disc, and be readable.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn require_tools_iso(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!(
            "cannot load storage/network drivers: tools ISO missing at {} (run `ternvale tools build-iso` or pass --tools-iso)",
            path.display()
        );
    }
    if path.is_dir() {
        return require_arm64_tree(path);
    }
    match disc_format(path) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => bail!(
            "cannot load storage/network drivers: {} is not an ISO 9660/UDF disc (Windows Setup would hang with no disk)",
            path.display()
        ),
        Err(error) => Err(error).with_context(|| format!("read tools ISO {}", path.display())),
    }
}

fn require_arm64_tree(root: &Path) -> Result<()> {
    let candidates = [
        root.join("virtio-win/viostor/w11/ARM64"),
        root.join("viostor/w11/ARM64"),
    ];
    if candidates.iter().any(|p| p.is_dir()) {
        return Ok(());
    }
    bail!(
        "cannot load storage drivers: {} has no ARM64 viostor (expected viostor/w11/ARM64); Windows Setup would hang with no disk",
        root.display()
    )
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
fn stage_arm64_drivers(tools: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).context("create drivers staging dir")?;
    if tools.is_dir() {
        copy_driver_tree(tools, dest)?;
        require_arm64_tree(dest)?;
        return Ok(());
    }
    for pattern in [
        "virtio-win/viostor/w11/ARM64",
        "virtio-win/NetKVM/w11/ARM64",
        "viostor/w11/ARM64",
        "NetKVM/w11/ARM64",
    ] {
        let output = Command::new("tar")
            .args(["-xf"])
            .arg(tools)
            .arg("-C")
            .arg(dest)
            .arg(pattern)
            .output()
            .context("spawn tar to extract ARM64 virtio drivers")?;
        if !output.status.success() {
            tracing::debug!(
                target: "ternvale::cli",
                pattern,
                status = %output.status,
                "optional driver path not on tools ISO"
            );
        }
    }
    let search = if dest.join("virtio-win").is_dir() {
        dest.join("virtio-win")
    } else {
        dest.to_path_buf()
    };
    require_arm64_tree(&search).or_else(|_| require_arm64_tree(dest))?;
    Ok(())
}

fn copy_driver_tree(src: &Path, dest: &Path) -> Result<()> {
    for rel in [
        "virtio-win/viostor/w11/ARM64",
        "virtio-win/NetKVM/w11/ARM64",
        "viostor/w11/ARM64",
        "NetKVM/w11/ARM64",
    ] {
        let from = src.join(rel);
        if !from.is_dir() {
            continue;
        }
        copy_dir(&from, &dest.join(rel))?;
    }
    Ok(())
}

fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry.context("read driver entry")?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let meta = entry
            .metadata()
            .with_context(|| format!("stat {}", from.display()))?;
        if meta.file_type().is_symlink() {
            tracing::warn!(target: "ternvale::cli", path = %from.display(), "skipping symlink in driver tree");
            continue;
        }
        if meta.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn unique_dir(prefix: &str) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}
