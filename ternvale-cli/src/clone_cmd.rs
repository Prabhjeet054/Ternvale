//! `ternvale clone` and `ternvale vm delete`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use ternvale_config::VmConfig;
use ternvale_snapshot::{create_linked_stack, list_snapshots, read_bundle, release_disk_base};

use crate::paths;

/// `ternvale clone <source-vm> <new-name>`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(source, new_name)
)]
pub fn clone_vm(source: &str, new_name: &str) -> Result<ExitCode> {
    if source == new_name {
        bail!("source and new VM name must differ");
    }
    let source_config_path = paths::vm_config_path(source)
        .with_context(|| format!("find config for source VM {source:?}"))?;
    if !source_config_path.is_file() {
        bail!(
            "no VM config at {} (expected ~/Library/Application Support/Ternvale/vms/{source}/config.toml)",
            source_config_path.display()
        );
    }
    let dest_dir = paths::vm_dir(new_name)?;
    if dest_dir.exists() {
        bail!("VM {new_name:?} already exists at {}", dest_dir.display());
    }
    let source_config = VmConfig::from_path(&source_config_path)
        .with_context(|| format!("load source VM config {}", source_config_path.display()))?;
    tracing::info!(
        target: "ternvale::cli",
        source,
        new_name,
        disks = source_config.disks.len(),
        "cloning vm"
    );
    fs::create_dir_all(dest_dir.join("disks")).context("create clone disks directory")?;
    let mut new_config = source_config.clone();
    new_config.name = new_name.to_string();
    new_config.serial_log = dest_dir.join("serial.log");
    new_config.nvram = Some(
        ternvale_config::default_nvram_path(new_name).context("default nvram path for clone")?,
    );
    for (index, disk) in new_config.disks.iter_mut().enumerate() {
        let stack_name = format!("disk{index}.tstack");
        let delta_name = format!("disk{index}.tdelta");
        let stack = create_linked_stack(
            &source_config.disks[index].path,
            &dest_dir.join("disks"),
            &stack_name,
            &delta_name,
            new_name,
        )
        .with_context(|| format!("clone disk {index} for VM {new_name}"))?;
        disk.path = stack;
        disk.read_only = false;
        tracing::info!(
            target: "ternvale::cli",
            index,
            stack = %disk.path.display(),
            "clone disk attached"
        );
    }
    ensure_source_base_refs(source, &source_config)?;
    let snap_note = copy_latest_vm_snapshot(source, new_name)?;
    let config_path = paths::vm_config_path(new_name)?;
    let text = new_config.to_toml().context("serialize cloned VM config")?;
    fs::write(&config_path, text).with_context(|| format!("write {}", config_path.display()))?;
    println!("cloned {source} -> {new_name}");
    println!("  config: {}", config_path.display());
    for disk in &new_config.disks {
        println!("  disk: {}", disk.path.display());
    }
    if let Some(note) = snap_note {
        println!("  vm snapshot: {}", note.display());
    }
    tracing::info!(
        target: "ternvale::cli",
        source,
        new_name,
        config = %config_path.display(),
        "vm clone complete"
    );
    Ok(ExitCode::SUCCESS)
}

/// `ternvale vm delete <name>`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(name))]
pub fn delete_vm(name: &str) -> Result<ExitCode> {
    let config_path = paths::vm_config_path(name)?;
    if !config_path.is_file() {
        bail!("no VM {:?} at {}", name, config_path.display());
    }
    let config = VmConfig::from_path(&config_path)
        .with_context(|| format!("load VM config {}", config_path.display()))?;
    for (index, disk) in config.disks.iter().enumerate() {
        release_disk_base(&disk.path, name)
            .with_context(|| format!("release disk {index} base refcount"))?;
    }
    let dir = paths::vm_dir(name)?;
    fs::remove_dir_all(&dir).with_context(|| format!("remove VM directory {}", dir.display()))?;
    let snap_dir = paths::snapshots_root()?.join(name);
    if snap_dir.is_dir() {
        fs::remove_dir_all(&snap_dir)
            .with_context(|| format!("remove VM snapshots {}", snap_dir.display()))?;
    }
    tracing::info!(
        target: "ternvale::cli",
        name,
        dir = %dir.display(),
        "vm deleted"
    );
    println!("deleted VM {name}");
    Ok(ExitCode::SUCCESS)
}

fn ensure_source_base_refs(source: &str, config: &VmConfig) -> Result<()> {
    use ternvale_snapshot::{acquire_base_ref, base_ref_has_holder, shared_base_path};
    for (index, disk) in config.disks.iter().enumerate() {
        let base = shared_base_path(&disk.path)
            .with_context(|| format!("resolve base for source disk {index}"))?;
        if !base_ref_has_holder(&base, source)? {
            acquire_base_ref(&base, source)
                .with_context(|| format!("register source hold on disk {index} base"))?;
        }
    }
    Ok(())
}

fn copy_latest_vm_snapshot(source: &str, dest: &str) -> Result<Option<PathBuf>> {
    let root = paths::snapshots_root()?;
    let labels = list_snapshots(&root, source)?;
    if labels.is_empty() {
        tracing::info!(target: "ternvale::cli", source, "no vm snapshot bundles to copy");
        return Ok(None);
    }
    let label = labels.last().expect("labels").clone();
    let src_bundle = paths::snapshot_bundle(source, &label)?;
    read_bundle(&src_bundle).context("validate source vm snapshot bundle")?;
    let dest_root = root.join(dest);
    fs::create_dir_all(&dest_root).context("create clone snapshot directory")?;
    let dest_bundle = dest_root.join(&label);
    copy_dir_recursive(&src_bundle, &dest_bundle)?;
    tracing::info!(
        target: "ternvale::cli",
        source,
        dest,
        label,
        bundle = %dest_bundle.display(),
        "copied latest vm snapshot bundle to clone"
    );
    Ok(Some(dest_bundle))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("mkdir {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("read dir {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::ExitCode;

    use ternvale_snapshot::{base_ref_count, shared_base_path};

    use crate::paths::{SNAPSHOTS_DIR_ENV, VMS_DIR_ENV};

    use super::*;

    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tv-clone-cmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    fn write_source_vm(root: &Path, name: &str) -> std::path::PathBuf {
        let vm_dir = root.join(name);
        std::fs::create_dir_all(vm_dir.join("disks")).expect("disks");
        let kernel = vm_dir.join("Image");
        std::fs::write(&kernel, b"ARM\x64fake").expect("kernel");
        let disk = vm_dir.join("disks").join("disk0.img");
        {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .open(&disk)
                .expect("disk");
            f.set_len(256 * 1024).expect("len");
        }
        let config = format!(
            r#"
name = "{name}"
cpus = 1
ram_mib = 256
kernel = "{kernel}"
serial_log = "{serial}"
cmdline = "console=ttyAMA0"

[[disks]]
path = "{disk}"
read_only = false
"#,
            name = name,
            kernel = kernel.display(),
            serial = vm_dir.join("serial.log").display(),
            disk = disk.display(),
        );
        let path = vm_dir.join("config.toml");
        std::fs::write(&path, config).expect("config");
        disk
    }

    #[test]
    fn clone_vm_shares_base_and_delete_drops_only_clone_hold() {
        let dir = scratch();
        unsafe {
            std::env::set_var(VMS_DIR_ENV, dir.join("vms"));
            std::env::set_var("TERNVALE_REFS_DIR", dir.join("refs"));
            std::env::set_var(SNAPSHOTS_DIR_ENV, dir.join("snapshots"));
        }
        let base_disk = write_source_vm(&dir.join("vms"), "source");
        assert_eq!(
            clone_vm("source", "child-one").expect("clone1"),
            ExitCode::SUCCESS
        );
        assert_eq!(
            clone_vm("source", "child-two").expect("clone2"),
            ExitCode::SUCCESS
        );
        assert_eq!(base_ref_count(&base_disk).expect("cnt"), 3);
        assert!(paths::vm_config_path("child-one").expect("p").is_file());
        let child_stack = paths::vm_dir("child-one")
            .expect("dir")
            .join("disks")
            .join("disk0.tstack");
        assert!(child_stack.is_file());
        let base = shared_base_path(&child_stack).expect("base");
        assert_eq!(
            base.canonicalize().expect("canon"),
            base_disk.canonicalize().expect("canon")
        );
        assert_eq!(delete_vm("child-one").expect("delete"), ExitCode::SUCCESS);
        assert_eq!(base_ref_count(&base_disk).expect("cnt"), 2);
        assert!(!paths::vm_dir("child-one").expect("dir").exists());
        assert!(base_disk.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
