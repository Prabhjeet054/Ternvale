//! `ternvale template create` and `ternvale new --from-template`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use ternvale_config::VmConfig;
use ternvale_snapshot::{create_linked_stack, shared_base_path};

use crate::json_out;
use crate::paths;
use crate::tvm_io;

/// Template sidecar magic.
const TEMPLATE_MAGIC: &str = "TERNVALE-TEMPLATE";
/// Sidecar version.
const TEMPLATE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct TemplateMeta {
    magic: String,
    version: u32,
    source_vm: String,
    created: String,
}

/// Freeze `vm` as read-only template `name`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, name))]
pub fn create(vm: &str, name: &str, json: bool) -> Result<ExitCode> {
    paths::socket_path(name)?;
    let src_config_path = paths::vm_config_path(vm)?;
    if !src_config_path.is_file() {
        bail!("no VM {vm:?} at {}", src_config_path.display());
    }
    let dest = paths::template_dir(name)?;
    if dest.exists() {
        bail!("template {name:?} already exists at {}", dest.display());
    }
    fs::create_dir_all(dest.join("disks")).context("create template disks directory")?;
    let src = VmConfig::from_path(&src_config_path)
        .with_context(|| format!("load {}", src_config_path.display()))?;
    tracing::info!(
        target: "ternvale::cli",
        vm,
        template = name,
        "creating read-only template"
    );
    let mut frozen = src.clone();
    frozen.name = name.to_string();
    frozen.serial_log = dest.join("serial.log");
    for (index, disk) in src.disks.iter().enumerate() {
        let base = shared_base_path(&disk.path)
            .with_context(|| format!("resolve template disk {index}"))?;
        let rel = format!("disks/disk{index}.img");
        let to = dest.join(&rel);
        tvm_io::copy_sparse(&base, &to).with_context(|| format!("copy template disk {index}"))?;
        let mut perm = fs::metadata(&to)
            .with_context(|| format!("stat {}", to.display()))?
            .permissions();
        perm.set_mode(0o444);
        fs::set_permissions(&to, perm).with_context(|| format!("chmod 0444 {}", to.display()))?;
        frozen.disks[index].path = to;
        frozen.disks[index].read_only = true;
    }
    if let Some(nvram) = src.nvram_path().context("nvram")? {
        if nvram.is_file() {
            let to = dest.join("nvram.fd");
            fs::copy(&nvram, &to).with_context(|| format!("copy nvram {}", nvram.display()))?;
            frozen.nvram = Some(to);
        }
    }
    let text = frozen.to_toml().context("serialize template config")?;
    fs::write(dest.join("config.toml"), text).context("write template config")?;
    let meta = TemplateMeta {
        magic: TEMPLATE_MAGIC.into(),
        version: TEMPLATE_VERSION,
        source_vm: vm.into(),
        created: chrono::Utc::now().to_rfc3339(),
    };
    fs::write(
        dest.join("TEMPLATE.json"),
        serde_json::to_string_pretty(&meta).context("encode template meta")?,
    )
    .context("write TEMPLATE.json")?;
    tracing::info!(
        target: "ternvale::cli",
        template = name,
        source_vm = vm,
        "template created"
    );
    json_out::emit(
        json,
        serde_json::json!({
            "ok": true,
            "template": name,
            "source_vm": vm,
            "path": dest.display().to_string(),
        }),
        || println!("template {name} created from {vm} (disks read-only)"),
    )?;
    Ok(ExitCode::SUCCESS)
}

/// Linked clone of template `template` as VM `name` (Step 54 stacks).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(template, name)
)]
pub fn new_from_template(name: &str, template: &str, json: bool) -> Result<ExitCode> {
    let tpl_dir = paths::template_dir(template)?;
    let tpl_config = tpl_dir.join("config.toml");
    if !tpl_config.is_file() {
        bail!("no template {template:?} at {}", tpl_config.display());
    }
    load_meta(&tpl_dir)?;
    let dest = paths::vm_dir(name)?;
    if dest.exists() {
        bail!("VM {name:?} already exists at {}", dest.display());
    }
    fs::create_dir_all(dest.join("disks")).context("create clone disks directory")?;
    let source = VmConfig::from_path(&tpl_config)
        .with_context(|| format!("load template {}", tpl_config.display()))?;
    tracing::info!(
        target: "ternvale::cli",
        template,
        vm = name,
        "linked clone from template"
    );
    let mut config = source.clone();
    config.name = name.to_string();
    config.serial_log = dest.join("serial.log");
    config.nvram =
        Some(ternvale_config::default_nvram_path(name).unwrap_or_else(|_| dest.join("nvram.fd")));
    for (index, disk) in config.disks.iter_mut().enumerate() {
        let src_disk = &source.disks[index].path;
        let stack = create_linked_stack(
            src_disk,
            &dest.join("disks"),
            &format!("disk{index}.tstack"),
            &format!("disk{index}.tdelta"),
            name,
        )
        .with_context(|| format!("clone template disk {index}"))?;
        disk.path = stack;
        disk.read_only = false;
    }
    if let Some(nvram) = source.nvram.as_ref() {
        if nvram.is_file() {
            if let Some(dest_nvram) = config.nvram.as_ref() {
                if let Some(parent) = dest_nvram.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("mkdir {}", parent.display()))?;
                }
                fs::copy(nvram, dest_nvram)
                    .with_context(|| format!("copy nvram to {}", dest_nvram.display()))?;
            }
        }
    }
    let config_path = paths::vm_config_path(name)?;
    let text = config.to_toml().context("serialize cloned config")?;
    fs::write(&config_path, text).with_context(|| format!("write {}", config_path.display()))?;
    tracing::info!(
        target: "ternvale::cli",
        vm = name,
        template,
        "linked clone from template complete"
    );
    json_out::emit(
        json,
        serde_json::json!({
            "ok": true,
            "vm": name,
            "template": template,
            "config": config_path.display().to_string(),
        }),
        || {
            println!("created {name} from template {template}");
            println!("  config: {}", config_path.display());
        },
    )?;
    Ok(ExitCode::SUCCESS)
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(dir = %dir.display()))]
fn load_meta(dir: &Path) -> Result<TemplateMeta> {
    let path = dir.join("TEMPLATE.json");
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let meta: TemplateMeta = serde_json::from_str(&text).context("parse TEMPLATE.json")?;
    if meta.magic != TEMPLATE_MAGIC {
        bail!("template magic {:?} is not {TEMPLATE_MAGIC:?}", meta.magic);
    }
    if meta.version != TEMPLATE_VERSION {
        tracing::warn!(
            target: "ternvale::cli",
            version = meta.version,
            "template version rejected"
        );
        bail!(
            "template version {} is not supported (this build reads {TEMPLATE_VERSION})",
            meta.version
        );
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_template_version() {
        let dir = std::env::temp_dir().join(format!("tpl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        fs::write(
            dir.join("TEMPLATE.json"),
            r#"{"magic":"TERNVALE-TEMPLATE","version":9,"source_vm":"a","created":"x"}"#,
        )
        .expect("write");
        let err = load_meta(&dir).unwrap_err();
        assert!(err.to_string().contains("version 9"), "{err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_template_and_linked_clone() {
        use crate::paths::{TEMPLATES_DIR_ENV, VMS_DIR_ENV};
        let dir = std::env::temp_dir().join(format!("tpl-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        // SAFETY: this test owns the library dirs; run with --test-threads=1.
        unsafe {
            std::env::set_var(VMS_DIR_ENV, dir.join("vms"));
            std::env::set_var(TEMPLATES_DIR_ENV, dir.join("templates"));
            std::env::set_var("TERNVALE_REFS_DIR", dir.join("refs"));
        }
        let vm = dir.join("vms").join("srcvm");
        std::fs::create_dir_all(vm.join("disks")).expect("disks");
        let kernel = vm.join("Image");
        std::fs::write(&kernel, b"k").expect("k");
        let disk = vm.join("disks").join("disk0.img");
        std::fs::write(&disk, vec![0u8; 128 * 1024]).expect("disk");
        let text = format!(
            r#"
name = "srcvm"
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
        assert_eq!(
            create("srcvm", "golden", false).expect("tpl"),
            ExitCode::SUCCESS
        );
        assert_eq!(
            new_from_template("clone", "golden", false).expect("clone"),
            ExitCode::SUCCESS
        );
        let stack = paths::vm_dir("clone")
            .expect("dir")
            .join("disks")
            .join("disk0.tstack");
        assert!(stack.is_file(), "expected linked-clone stack");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
