//! Clone a macOS VZ guest: copy the disk, mint a new machine identifier.

use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};

use anyhow::{bail, Context, Result};
use ternvale_config::{Profile, VmConfig};

use crate::paths;

/// Copy disks with APFS clonefile when possible, then a new VZ machine id.
#[tracing::instrument(
    level = "info",
    target = "ternvale::cli",
    skip_all,
    fields(source, new_name)
)]
pub fn clone_macos_vm(source: &str, new_name: &str, source_config: &VmConfig) -> Result<ExitCode> {
    if source_config.profile != Profile::Macos {
        bail!("internal: clone_macos_vm on a non-macos config");
    }
    let dest_dir = paths::vm_dir(new_name)?;
    fs::create_dir_all(dest_dir.join("disks")).context("create clone disks directory")?;
    let src_dir = paths::vm_dir(source)?;
    let mut new_config = source_config.clone();
    new_config.name = new_name.to_string();
    new_config.serial_log = dest_dir.join("serial.log");
    for (index, disk) in new_config.disks.iter_mut().enumerate() {
        let dest_disk = dest_dir.join("disks").join(format!("disk{index}.img"));
        cow_copy(&source_config.disks[index].path, &dest_disk)
            .with_context(|| format!("copy macos disk {index}"))?;
        disk.path = dest_disk;
        disk.read_only = false;
    }
    ternvale_backend::clone_macos_platform(&src_dir, &dest_dir)
        .context("clone macos platform identity (new machineIdentifier)")?;
    let config_path = paths::vm_config_path(new_name)?;
    let text = new_config
        .to_toml()
        .context("serialize cloned macos config")?;
    fs::write(&config_path, text).with_context(|| format!("write {}", config_path.display()))?;
    println!("cloned {source} -> {new_name} (new macOS machine identifier)");
    println!("  config: {}", config_path.display());
    for disk in &new_config.disks {
        println!("  disk: {}", disk.path.display());
    }
    tracing::info!(
        target: "ternvale::cli",
        source,
        new_name,
        config = %config_path.display(),
        "macos vm clone complete"
    );
    Ok(ExitCode::SUCCESS)
}

/// `cp -c` is clonefile on APFS; fall back to a full copy.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(src = %src.display(), dst = %dst.display()))]
pub fn cow_copy(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).context("create clone parent")?;
    }
    let status = Command::new("cp")
        .args([
            "-c",
            "--",
            &src.display().to_string(),
            &dst.display().to_string(),
        ])
        .status()
        .context("spawn cp -c")?;
    if status.success() {
        tracing::info!(
            target: "ternvale::cli",
            src = %src.display(),
            dst = %dst.display(),
            "copied macos disk with clonefile"
        );
        return Ok(());
    }
    tracing::warn!(
        target: "ternvale::cli",
        src = %src.display(),
        dst = %dst.display(),
        status = ?status,
        "cp -c failed; using a full copy"
    );
    fs::copy(src, dst).with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cow_copy_round_trip() {
        let dir = std::env::temp_dir().join(format!("tv-cow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let src = dir.join("a.bin");
        let dst = dir.join("b.bin");
        std::fs::write(&src, b"hello-clone").expect("src");
        cow_copy(&src, &dst).expect("copy");
        assert_eq!(std::fs::read(&dst).expect("dst"), b"hello-clone");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
