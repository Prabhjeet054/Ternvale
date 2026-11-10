//! `ternvale import <file.tvm|disk.qcow2>`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use ternvale_config::VmConfig;
use ternvale_snapshot::create_linked_stack;

use crate::json_out;
use crate::paths;
use crate::qemu_img;
use crate::tvm::{TvmManifest, MANIFEST};
use crate::tvm_io;

/// Import a `.tvm` directory or a qcow2/vmdk/vhdx/raw disk.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(src = %src.display(), name, layered)
)]
pub fn import(
    src: &Path,
    name: Option<&str>,
    layered: bool,
    kernel: Option<&Path>,
    firmware: Option<&Path>,
    json: bool,
) -> Result<ExitCode> {
    if qemu_img::is_foreign_disk(src) {
        return import_disk(src, name, layered, kernel, firmware, json);
    }
    import_tvm(src, name, json)
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(src = %src.display()))]
fn import_tvm(src: &Path, name: Option<&str>, json: bool) -> Result<ExitCode> {
    let manifest_path = src.join(MANIFEST);
    let text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest = TvmManifest::parse(&text).map_err(|error| anyhow::anyhow!("{error}"))?;
    let name = name.unwrap_or(&manifest.source_vm);
    paths::socket_path(name)?;
    let dest = paths::vm_dir(name)?;
    if dest.exists() {
        bail!("VM {name:?} already exists at {}", dest.display());
    }
    fs::create_dir_all(&dest).with_context(|| format!("mkdir {}", dest.display()))?;
    tracing::info!(
        target: "ternvale::cli",
        vm = name,
        src = %src.display(),
        files = manifest.files.len(),
        "importing tvm bundle"
    );
    for entry in &manifest.files {
        let from = src.join(&entry.path);
        let to = dest.join(&entry.path);
        tvm_io::restore(&from, &to, entry)?;
    }
    let config_path = dest.join("config.toml");
    let text = fs::read_to_string(&config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    let mut config = VmConfig::parse(&text).context("parse imported config")?;
    rewrite_imported(&mut config, name, &dest);
    let text = config.to_toml().context("serialize imported config")?;
    fs::write(&config_path, text).with_context(|| format!("write {}", config_path.display()))?;
    tracing::info!(
        target: "ternvale::cli",
        vm = name,
        path = %dest.display(),
        "tvm import complete"
    );
    json_out::emit(
        json,
        serde_json::json!({
            "ok": true,
            "vm": name,
            "path": dest.display().to_string(),
            "files": manifest.files.len(),
        }),
        || {
            println!("imported {} -> {name}", src.display());
            println!("  config: {}", config_path.display());
        },
    )?;
    Ok(ExitCode::SUCCESS)
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name))]
fn rewrite_imported(config: &mut VmConfig, name: &str, dest: &Path) {
    config.name = name.to_string();
    config.serial_log = dest.join("serial.log");
    if config.kernel.as_os_str() == "boot/kernel" || !config.kernel.is_absolute() {
        let boot = dest.join(&config.kernel);
        if boot.is_file() {
            config.kernel = boot;
        }
    }
    if let Some(initrd) = config.initrd.clone() {
        let path = dest.join(&initrd);
        if path.is_file() {
            config.initrd = Some(path);
        }
    }
    if let Some(firmware) = config.firmware.clone() {
        let path = dest.join(&firmware);
        if path.is_file() {
            config.firmware = Some(path);
        }
    }
    if let Some(nvram) = config.nvram.clone() {
        config.nvram = Some(dest.join(nvram));
    }
    for disk in &mut config.disks {
        if !disk.path.is_absolute() {
            disk.path = dest.join(&disk.path);
        }
    }
}

#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(src = %src.display(), layered)
)]
fn import_disk(
    src: &Path,
    name: Option<&str>,
    layered: bool,
    kernel: Option<&Path>,
    firmware: Option<&Path>,
    json: bool,
) -> Result<ExitCode> {
    let name =
        name.ok_or_else(|| anyhow::anyhow!("--name is required when importing a disk image"))?;
    paths::socket_path(name)?;
    if kernel.is_none() && firmware.is_none() {
        bail!("importing a disk image needs --kernel PATH or --firmware PATH");
    }
    let dest = paths::vm_dir(name)?;
    if dest.exists() {
        bail!("VM {name:?} already exists at {}", dest.display());
    }
    fs::create_dir_all(dest.join("disks")).context("create imported disks directory")?;
    let raw = dest.join("disks").join("disk0.img");
    tracing::info!(
        target: "ternvale::cli",
        vm = name,
        src = %src.display(),
        layered,
        "importing foreign disk"
    );
    qemu_img::convert_to_raw(src, &raw)?;
    let disk_path = if layered {
        create_linked_stack(
            &raw,
            &dest.join("disks"),
            "disk0.tstack",
            "disk0.tdelta",
            name,
        )
        .context("create layered disk on imported raw")?
    } else {
        raw.clone()
    };
    let kernel_path = kernel
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/nonexistent/Image"));
    let mut config = format!(
        "name = \"{name}\"\ncpus = 1\nram_mib = 256\nserial_log = \"{}\"\n",
        dest.join("serial.log").display()
    );
    if let Some(firmware) = firmware {
        config.push_str(&format!("firmware = \"{}\"\n", firmware.display()));
        if !kernel_path.as_os_str().is_empty() && kernel.is_some() {
            config.push_str(&format!("kernel = \"{}\"\n", kernel_path.display()));
        }
    } else {
        config.push_str(&format!("kernel = \"{}\"\n", kernel_path.display()));
    }
    config.push_str(&format!(
        "\n[[disks]]\npath = \"{}\"\nread_only = false\n",
        disk_path.display()
    ));
    let config_path = dest.join("config.toml");
    fs::write(&config_path, config).with_context(|| format!("write {}", config_path.display()))?;
    let loaded = VmConfig::from_path(&config_path)
        .with_context(|| format!("validate imported config {}", config_path.display()))?;
    json_out::emit(
        json,
        serde_json::json!({
            "ok": true,
            "vm": loaded.name,
            "path": dest.display().to_string(),
            "disk": disk_path.display().to_string(),
            "layered": layered,
        }),
        || {
            println!("imported disk {} -> {name}", src.display());
            println!("  disk: {}", disk_path.display());
            println!(
                "  note: imported Windows disks may need virtio drivers and may trigger Windows re-activation"
            );
        },
    )?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::VMS_DIR_ENV;
    use crate::tvm::{Encoding, FileKind, TvmFile};

    #[test]
    fn checksum_mismatch_names_the_file() {
        let dir = std::env::temp_dir().join(format!("tvm-imp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let src = dir.join("a.bin");
        std::fs::write(&src, b"hello").expect("write");
        let entry = TvmFile {
            path: "disks/disk0.img".into(),
            sha256: "0".repeat(64),
            bytes: 5,
            logical_sha256: "0".repeat(64),
            logical_bytes: 5,
            encoding: Encoding::Raw,
            kind: FileKind::Disk,
        };
        let err = tvm_io::restore(&src, &dir.join("out.bin"), &entry).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("disks/disk0.img"), "{msg}");
        assert!(msg.contains("checksum mismatch"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = VMS_DIR_ENV;
    }
}
