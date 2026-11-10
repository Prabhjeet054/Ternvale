//! `ternvale export <vm> <file.tvm>`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use ternvale_config::VmConfig;
use ternvale_snapshot::{is_stack_manifest, StackManifest};

use crate::client::{self, Reach};
use crate::json_out;
use crate::paths;
use crate::tvm::{FileKind, TvmFile, TvmManifest, MANIFEST};
use crate::tvm_io;

/// Export library VM `name` to directory bundle `out` (`.tvm`).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(vm = name, out = %out.display(), zstd)
)]
pub fn export_vm(name: &str, out: &Path, zstd: bool, json: bool) -> Result<ExitCode> {
    let config_path = paths::vm_config_path(name)?;
    if !config_path.is_file() {
        bail!("no VM {name:?} at {}", config_path.display());
    }
    if let Ok(Ok(_)) = client::connect(&paths::socket_path(name)?) {
        bail!("vm {name} is running; stop it before export");
    } else if let Ok(Err(Reach::Stale)) = client::connect(&paths::socket_path(name)?) {
        tracing::warn!(target: "ternvale::cli", vm = name, "stale control socket; exporting files anyway");
    }
    if out.exists() {
        bail!("refusing to overwrite {}", out.display());
    }
    fs::create_dir_all(out).with_context(|| format!("mkdir {}", out.display()))?;
    let config = VmConfig::from_path(&config_path)
        .with_context(|| format!("load {}", config_path.display()))?;
    tracing::info!(
        target: "ternvale::cli",
        vm = name,
        out = %out.display(),
        zstd,
        "exporting vm bundle"
    );
    let mut files: Vec<TvmFile> = Vec::new();
    let mut exported = config.clone();
    exported.serial_log = PathBuf::from("serial.log");
    if let Some(kernel) = copy_boot(&config.kernel, out, "boot/kernel", zstd, &mut files)? {
        exported.kernel = PathBuf::from(kernel);
    }
    if let Some(initrd) = config.initrd.as_ref() {
        if let Some(rel) = copy_boot(initrd, out, "boot/initrd", zstd, &mut files)? {
            exported.initrd = Some(PathBuf::from(rel));
        }
    }
    if let Some(firmware) = config.firmware.as_ref() {
        if let Some(rel) = copy_boot(firmware, out, "boot/firmware.fd", zstd, &mut files)? {
            exported.firmware = Some(PathBuf::from(rel));
        }
    }
    for (index, disk) in config.disks.iter().enumerate() {
        let rel = export_disk(&disk.path, out, index, zstd, &mut files)?;
        exported.disks[index].path = PathBuf::from(rel);
    }
    if let Some(nvram) = config.nvram_path().context("nvram path")? {
        if nvram.is_file() {
            files.push(tvm_io::store(
                &nvram,
                &out.join("nvram.fd"),
                "nvram.fd",
                FileKind::Nvram,
                zstd,
            )?);
            exported.nvram = Some(PathBuf::from("nvram.fd"));
        }
        if let Some(tpm) = config.tpm_state_path().context("tpm state")? {
            if tpm.is_file() {
                files.push(tvm_io::store(
                    &tpm,
                    &out.join("nvram.fd.swtpm"),
                    "nvram.fd.swtpm",
                    FileKind::Tpm,
                    zstd,
                )?);
            }
        }
    }
    // Relative paths inside the bundle are not valid until import rewrites them.
    let config_text =
        toml::to_string_pretty(&exported).context("serialize exported config (unvalidated)")?;
    let config_rel = "config.toml";
    fs::write(out.join(config_rel), &config_text)
        .with_context(|| format!("write {}/{config_rel}", out.display()))?;
    files.push(tvm_io::describe(
        &out.join(config_rel),
        config_rel,
        FileKind::Config,
    )?);
    let manifest = TvmManifest::new(name.to_string(), files);
    let body = serde_json::to_string_pretty(&manifest).context("encode tvm manifest")?;
    fs::write(out.join(MANIFEST), body)
        .with_context(|| format!("write {}/{MANIFEST}", out.display()))?;
    tracing::info!(
        target: "ternvale::cli",
        vm = name,
        files = manifest.files.len(),
        "tvm export complete"
    );
    json_out::emit(
        json,
        serde_json::json!({
            "ok": true,
            "vm": name,
            "path": out.display().to_string(),
            "files": manifest.files.len(),
            "zstd": manifest.zstd,
        }),
        || {
            println!("exported {name} -> {}", out.display());
            println!("  files: {}", manifest.files.len());
        },
    )?;
    Ok(ExitCode::SUCCESS)
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(rel))]
fn copy_boot(
    src: &Path,
    bundle: &Path,
    rel: &str,
    zstd: bool,
    files: &mut Vec<TvmFile>,
) -> Result<Option<String>> {
    if src.as_os_str().is_empty() || !src.is_file() {
        return Ok(None);
    }
    files.push(tvm_io::store(
        src,
        &bundle.join(rel),
        rel,
        FileKind::Boot,
        zstd,
    )?);
    Ok(Some(rel.to_string()))
}

#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(index, src = %src.display())
)]
fn export_disk(
    src: &Path,
    bundle: &Path,
    index: usize,
    zstd: bool,
    files: &mut Vec<TvmFile>,
) -> Result<String> {
    if is_stack_manifest(src) {
        let stack =
            StackManifest::load(src).with_context(|| format!("load stack {}", src.display()))?;
        let base_rel = format!("disks/disk{index}-base.img");
        files.push(tvm_io::store(
            &stack.base,
            &bundle.join(&base_rel),
            &base_rel,
            FileKind::Disk,
            zstd,
        )?);
        let mut deltas = Vec::new();
        for (i, delta) in stack.deltas.iter().enumerate() {
            let rel = format!("disks/disk{index}-d{i}.tdelta");
            files.push(tvm_io::store(
                delta,
                &bundle.join(&rel),
                &rel,
                FileKind::DiskLayer,
                zstd,
            )?);
            deltas.push(bundle.join(&rel));
        }
        let stack_rel = format!("disks/disk{index}.tstack");
        let written = StackManifest {
            base: bundle.join(&base_rel),
            deltas,
        };
        written
            .save(&bundle.join(&stack_rel))
            .with_context(|| format!("write {stack_rel}"))?;
        files.push(tvm_io::describe(
            &bundle.join(&stack_rel),
            &stack_rel,
            FileKind::DiskLayer,
        )?);
        Ok(stack_rel)
    } else {
        let rel = format!("disks/disk{index}.img");
        files.push(tvm_io::store(
            src,
            &bundle.join(&rel),
            &rel,
            FileKind::Disk,
            zstd,
        )?);
        Ok(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import_cmd;
    use crate::paths::VMS_DIR_ENV;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tvm-ex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    fn write_vm(root: &Path, name: &str) {
        let vm = root.join(name);
        std::fs::create_dir_all(vm.join("disks")).expect("disks");
        let kernel = vm.join("Image");
        std::fs::write(&kernel, b"ARM64kernel").expect("kernel");
        let disk = vm.join("disks").join("disk0.img");
        {
            let mut f = std::fs::File::create(&disk).expect("disk");
            use std::io::{Seek, SeekFrom, Write};
            f.set_len(128 * 1024).expect("len");
            f.write_all(b"head").expect("h");
            f.seek(SeekFrom::Start(128 * 1024 - 4)).expect("s");
            f.write_all(b"tail").expect("t");
        }
        let text = format!(
            r#"
name = "{name}"
cpus = 1
ram_mib = 256
kernel = "{kernel}"
serial_log = "{serial}"

[[disks]]
path = "{disk}"
read_only = false
"#,
            kernel = kernel.display(),
            serial = vm.join("serial.log").display(),
            disk = disk.display(),
        );
        std::fs::write(vm.join("config.toml"), text).expect("cfg");
    }

    #[test]
    fn export_import_roundtrip_and_checksum_reject() {
        let dir = scratch();
        // SAFETY: this test owns the library dir; run with --test-threads=1.
        unsafe {
            std::env::set_var(VMS_DIR_ENV, dir.join("vms"));
        }
        write_vm(&dir.join("vms"), "srcvm");
        let bundle = dir.join("srcvm.tvm");
        assert_eq!(
            export_vm("srcvm", &bundle, false, false).expect("export"),
            ExitCode::SUCCESS
        );
        assert!(bundle.join(MANIFEST).is_file());
        assert_eq!(
            import_cmd::import(&bundle, Some("copy"), false, None, None, false).expect("import"),
            ExitCode::SUCCESS
        );
        let imported = paths::vm_dir("copy").expect("dir").join("disks/disk0.img");
        let original = dir.join("vms/srcvm/disks/disk0.img");
        assert_eq!(
            crate::tvm_io::sha256_path(&original).expect("a"),
            crate::tvm_io::sha256_path(&imported).expect("b")
        );
        let payload = bundle.join("disks/disk0.img");
        let mut bytes = std::fs::read(&payload).expect("read");
        bytes[0] ^= 0xff;
        std::fs::write(&payload, bytes).expect("corrupt");
        let err = import_cmd::import(&bundle, Some("bad"), false, None, None, false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("checksum mismatch"), "{msg}");
        assert!(msg.contains("disks/disk0.img"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
