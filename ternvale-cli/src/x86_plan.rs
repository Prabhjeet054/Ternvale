//! `ternvale x86 plan` / `setup`: engine the app toggle uses (capability flags, not backend names).

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use ternvale_backend::BackendCapabilities;
use ternvale_compat::X86Engine;
use ternvale_config::{Profile, VmConfig, X86Emulation};

use crate::fex_cmd;
use crate::guest_setup;
use crate::json_out;
use crate::paths;
use crate::qemu_cmd;

/// JSON the Swift settings pane decodes. No `backend = "vz"` field.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct X86Plan {
    pub ok: bool,
    pub vm: String,
    pub profile: String,
    pub requested: String,
    pub engine: String,
    pub enabled: bool,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub setup_kind: String,
    pub setup_hint: String,
    pub needs_running_vm: bool,
    pub caps: PlanCaps,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlanCaps {
    pub rosetta: bool,
    pub fex: bool,
    pub qemu_user: bool,
}

/// `ternvale x86 plan --config`.
#[tracing::instrument(level = "info", target = "ternvale::compat", skip_all, fields(config = %path.display()))]
pub fn plan(path: &Path, json: bool) -> Result<ExitCode> {
    let plan = compute(path)?;
    emit_plan(&plan, json)?;
    Ok(ExitCode::SUCCESS)
}

/// `ternvale x86 setup --config` (Step 97 guest-setup / 98 FEX RootFS / 99 qemu-user).
#[tracing::instrument(level = "info", target = "ternvale::compat", skip_all, fields(config = %path.display(), yes))]
pub fn setup(path: &Path, yes: bool, json: bool) -> Result<ExitCode> {
    match setup_inner(path, yes, json) {
        Ok(code) => Ok(code),
        Err(error) => {
            let logs = paths::host_log_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "~/Library/Logs/Ternvale".into());
            tracing::error!(
                target: "ternvale::compat",
                logs = %logs,
                error = %error,
                "x86 setup failed"
            );
            Err(error.context(format!("x86 setup failed (host logs: {logs})")))
        }
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all, fields(config = %path.display(), yes))]
fn setup_inner(path: &Path, yes: bool, json: bool) -> Result<ExitCode> {
    let plan = compute(path)?;
    tracing::info!(
        target: "ternvale::compat",
        vm = %plan.vm,
        engine = %plan.engine,
        setup_kind = %plan.setup_kind,
        "x86 one-click setup"
    );
    match plan.setup_kind.as_str() {
        "none" => {
            json_out::emit(
                json,
                serde_json::json!({"ok": true, "vm": plan.vm, "setup_kind": "none"}),
                || println!("{}: nothing to install (engine={})", plan.vm, plan.engine),
            )?;
            Ok(ExitCode::SUCCESS)
        }
        "windows-woa" => {
            json_out::emit(
                json,
                serde_json::json!({"ok": true, "vm": plan.vm, "setup_kind": "windows-woa"}),
                || {
                    println!(
                        "{}: Windows on ARM provides x64 emulation inside the guest; Ternvale does not control it",
                        plan.vm
                    );
                },
            )?;
            Ok(ExitCode::SUCCESS)
        }
        "fex-rootfs" => fex_cmd::fetch_rootfs("Ubuntu_24_04", None, yes, json),
        "rosetta-guest-setup" => guest_setup::guest_setup(&plan.vm, false, false, json),
        "qemu-user" => qemu_cmd::install_qemu_user(Some(&plan.vm), None, false, yes, json),
        other => bail!("unknown setup_kind {other}"),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all, fields(config = %path.display()))]
pub fn compute(path: &Path) -> Result<X86Plan> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read VM config {}", path.display()))?;
    let config =
        VmConfig::parse(&text).with_context(|| format!("parse VM config {}", path.display()))?;
    let selection = ternvale_backend::resolve(&config);
    let caps = BackendCapabilities::for_guest(selection.resolved, config.profile);
    let want = ternvale_compat::requested(config.x86_emulation, config.rosetta);
    let enabled = !want.is_none();
    let engine = match ternvale_compat::resolve(want, caps.x86(), config.profile) {
        Ok(engine) => engine,
        Err(error) => {
            tracing::warn!(
                target: "ternvale::compat",
                vm = %config.name,
                error = %error,
                "x86 plan: engine not advertised"
            );
            return Ok(plan_from(
                &config,
                want,
                X86Engine::None,
                enabled,
                false,
                Some(error.to_string()),
                caps,
            ));
        }
    };
    let fex_ok = fex_rootfs_present()?;
    let (available, reason, setup_kind, hint, needs_vm) =
        status(config.profile, engine, enabled, caps, fex_ok);
    tracing::info!(
        target: "ternvale::compat",
        vm = %config.name,
        engine = engine.as_str(),
        available,
        setup_kind,
        "x86 plan"
    );
    Ok(X86Plan {
        ok: true,
        vm: config.name.clone(),
        profile: config.profile.to_string(),
        requested: want.as_str().to_string(),
        engine: engine.as_str().to_string(),
        enabled,
        available,
        unavailable_reason: reason,
        setup_kind: setup_kind.to_string(),
        setup_hint: hint,
        needs_running_vm: needs_vm,
        caps: PlanCaps {
            rosetta: caps.rosetta,
            fex: caps.fex,
            qemu_user: caps.qemu_user,
        },
    })
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all)]
fn plan_from(
    config: &VmConfig,
    want: X86Emulation,
    engine: X86Engine,
    enabled: bool,
    available: bool,
    reason: Option<String>,
    caps: BackendCapabilities,
) -> X86Plan {
    let (setup_kind, hint, needs_vm) = if config.profile == Profile::Windows {
        (
            "windows-woa",
            "Windows on ARM provides x64 emulation inside the guest; Ternvale does not control it."
                .to_string(),
            false,
        )
    } else {
        (
            "none",
            reason
                .clone()
                .unwrap_or_else(|| "No x86 usermode engine is advertised.".into()),
            false,
        )
    };
    X86Plan {
        ok: true,
        vm: config.name.clone(),
        profile: config.profile.to_string(),
        requested: want.as_str().to_string(),
        engine: engine.as_str().to_string(),
        enabled,
        available,
        unavailable_reason: reason,
        setup_kind: setup_kind.to_string(),
        setup_hint: hint,
        needs_running_vm: needs_vm,
        caps: PlanCaps {
            rosetta: caps.rosetta,
            fex: caps.fex,
            qemu_user: caps.qemu_user,
        },
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all)]
fn status(
    profile: Profile,
    engine: X86Engine,
    enabled: bool,
    caps: BackendCapabilities,
    fex_ok: bool,
) -> (bool, Option<String>, &'static str, String, bool) {
    if profile == Profile::Windows {
        return (
            false,
            Some(
                "Windows on ARM provides x64 emulation inside the guest; Ternvale does not control it"
                    .into(),
            ),
            "windows-woa",
            "Use x64 apps in the guest (7-Zip x64, a portable editor, .NET x64). Ternvale cannot install that emulator.".into(),
            false,
        );
    }
    if !enabled {
        return (
            false,
            Some("Run x86 programs is off (`x86_emulation = \"none\"`)".into()),
            "none",
            "Turn the toggle on to set x86_emulation = \"auto\".".into(),
            false,
        );
    }
    match engine {
        X86Engine::None => {
            let reason = if !caps.rosetta && !caps.fex && !caps.qemu_user {
                "this backend does not advertise an x86 usermode engine (native Hypervisor.framework needs FEX or qemu-user; Apple Linux Rosetta is a capability flag, not a backend name)"
            } else {
                "no x86 engine was selected"
            };
            (
                false,
                Some(reason.into()),
                "none",
                reason.into(),
                false,
            )
        }
        X86Engine::Fex if !fex_ok => (
            false,
            Some("native path without a FEX RootFS installed (`ternvale x86 fetch-rootfs`)".into()),
            "fex-rootfs",
            "One-click setup downloads a FEX-Emu Ubuntu RootFS (not bundled; type yes / --yes).".into(),
            false,
        ),
        X86Engine::Fex => (
            true,
            None,
            "fex-rootfs",
            "FEX RootFS is present. Re-run setup to fetch again if you need another distro.".into(),
            false,
        ),
        X86Engine::Rosetta => (
            true,
            None,
            "rosetta-guest-setup",
            "Apple Linux Rosetta is advertised. Start the VM and run guest-setup (Step 97).".into(),
            true,
        ),
        X86Engine::QemuUser => (
            true,
            None,
            "qemu-user",
            "qemu-user-static is GPL and not bundled. Start the VM; setup installs the distro package.".into(),
            true,
        ),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all)]
fn fex_rootfs_present() -> Result<bool> {
    let dir = paths::fex_rootfs_dir()?;
    if !dir.is_dir() {
        return Ok(false);
    }
    for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry.context("FEX RootFS dirent")?.path();
        if let Some("sqsh" | "squashfs" | "ero" | "erofs") =
            path.extension().and_then(|e| e.to_str())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all)]
fn emit_plan(plan: &X86Plan, json: bool) -> Result<()> {
    json_out::emit(json, plan, || {
        println!(
            "{}: engine={} available={}",
            plan.vm, plan.engine, plan.available
        );
        if let Some(reason) = &plan.unavailable_reason {
            println!("  unavailable: {reason}");
        }
        println!("  setup: {} — {}", plan.setup_kind, plan.setup_hint);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV: Mutex<()> = Mutex::new(());

    fn write_cfg(dir: &Path, body: &str) -> std::path::PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, body).unwrap();
        path
    }

    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tv-x86-plan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn linux_auto(name: &str) -> String {
        format!(
            "name = \"{name}\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\nx86_emulation = \"auto\"\n"
        )
    }

    #[test]
    fn native_auto_without_rootfs_is_unavailable_fex() {
        let _g = ENV.lock().unwrap();
        let dir = scratch();
        let empty = dir.join("rootfs");
        fs::create_dir_all(&empty).unwrap();
        std::env::set_var(paths::FEX_ROOTFS_DIR_ENV, &empty);
        let cfg = write_cfg(&dir, &linux_auto("lab"));
        let plan = compute(&cfg).unwrap();
        std::env::remove_var(paths::FEX_ROOTFS_DIR_ENV);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(plan.engine, "fex");
        assert!(!plan.available);
        assert_eq!(plan.setup_kind, "fex-rootfs");
        assert!(plan
            .unavailable_reason
            .as_deref()
            .unwrap()
            .contains("FEX RootFS"));
        assert!(!plan.caps.rosetta);
        assert!(plan.caps.fex);
    }

    #[test]
    fn native_auto_with_squashfs_is_available() {
        let _g = ENV.lock().unwrap();
        let dir = scratch();
        let rootfs = dir.join("rootfs");
        fs::create_dir_all(&rootfs).unwrap();
        fs::write(rootfs.join("Ubuntu_24_04.sqsh"), b"x").unwrap();
        std::env::set_var(paths::FEX_ROOTFS_DIR_ENV, &rootfs);
        let cfg = write_cfg(&dir, &linux_auto("lab"));
        let plan = compute(&cfg).unwrap();
        std::env::remove_var(paths::FEX_ROOTFS_DIR_ENV);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(plan.engine, "fex");
        assert!(plan.available);
        assert!(plan.unavailable_reason.is_none());
    }

    #[test]
    fn windows_notes_woa_not_ternvale() {
        let _g = ENV.lock().unwrap();
        let dir = scratch();
        std::env::set_var(paths::FEX_ROOTFS_DIR_ENV, dir.join("empty"));
        let cfg = write_cfg(
            &dir,
            "name = \"win\"\nprofile = \"windows\"\ncpus = 2\nram_mib = 4096\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n",
        );
        let plan = compute(&cfg).unwrap();
        std::env::remove_var(paths::FEX_ROOTFS_DIR_ENV);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(plan.setup_kind, "windows-woa");
        assert!(!plan.available);
        assert!(plan
            .unavailable_reason
            .as_deref()
            .unwrap()
            .contains("Windows on ARM"));
    }

    #[test]
    fn macos_has_no_usermode_engine() {
        let _g = ENV.lock().unwrap();
        let dir = scratch();
        std::env::set_var(paths::FEX_ROOTFS_DIR_ENV, dir.join("empty"));
        let cfg = write_cfg(
            &dir,
            "name = \"mac\"\nprofile = \"macos\"\nbackend = \"vz\"\ncpus = 2\nram_mib = 4096\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\nx86_emulation = \"auto\"\n",
        );
        let plan = compute(&cfg).unwrap();
        std::env::remove_var(paths::FEX_ROOTFS_DIR_ENV);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(plan.engine, "none");
        assert!(!plan.available);
        assert_eq!(plan.setup_kind, "none");
        assert!(!plan.caps.fex);
        assert!(!plan.caps.rosetta);
    }

    #[test]
    fn linux_vz_auto_selects_rosetta() {
        let _g = ENV.lock().unwrap();
        let dir = scratch();
        std::env::set_var(paths::FEX_ROOTFS_DIR_ENV, dir.join("empty"));
        let cfg = write_cfg(
            &dir,
            "name = \"vzlab\"\nbackend = \"vz\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\nx86_emulation = \"auto\"\n",
        );
        let plan = compute(&cfg).unwrap();
        std::env::remove_var(paths::FEX_ROOTFS_DIR_ENV);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(plan.engine, "rosetta");
        assert!(plan.available);
        assert_eq!(plan.setup_kind, "rosetta-guest-setup");
        assert!(plan.caps.rosetta);
        assert!(plan.needs_running_vm);
    }

    #[test]
    fn setup_stopped_qemu_user_points_at_host_logs() {
        let _g = ENV.lock().unwrap();
        let dir = scratch();
        std::env::set_var(paths::FEX_ROOTFS_DIR_ENV, dir.join("empty"));
        let cfg = write_cfg(
            &dir,
            "name = \"deadx86\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\nx86_emulation = \"qemu-user\"\n",
        );
        let err = setup(&cfg, true, true).unwrap_err();
        std::env::remove_var(paths::FEX_ROOTFS_DIR_ENV);
        let _ = fs::remove_dir_all(&dir);
        let text = format!("{err:#}");
        assert!(
            text.contains("host logs") || text.contains("Library/Logs/Ternvale"),
            "{text}"
        );
        assert!(
            text.contains("not running") || text.contains("qemu-user"),
            "{text}"
        );
    }
}
