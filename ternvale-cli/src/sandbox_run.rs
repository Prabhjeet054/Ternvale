//! Unsandboxed launcher: bookmarks, swtpm, then Seatbelt child.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use ternvale_config::{FirmwareTables, TpmBackendKind, VmConfig};
use ternvale_sandbox::{
    hold_bookmarks, is_sandboxed, maybe_relaunch, probe_requested, run_debug_probe, skip_sandbox,
    SANDBOXED_ENV,
};
use ternvale_tpm::{find_swtpm, SwtpmDaemon, SWTPM_CTRL_ENV, SWTPM_DATA_ENV};

use crate::display::DisplayMode;

/// What the unsandboxed launcher still holds while the VM runs (or after relaunch).
pub struct LaunchHold {
    /// Exit status when this process only waited for a Seatbelt child.
    pub child_exit: Option<ExitCode>,
    /// swtpm reaped after the child exits.
    pub swtpm: Option<SwtpmDaemon>,
    /// Live security-scoped access in the confined VMM.
    pub bookmarks: Option<Vec<ternvale_hv::ScopedAccess>>,
}

/// If this process is the launcher, confine the VMM and wait. `child_exit` means do not boot here.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn prepare(config_path: &Path, mode: DisplayMode, fps: u32) -> Result<LaunchHold> {
    if std::env::var(SANDBOXED_ENV).is_ok_and(|v| v == "1") {
        let held = hold_bookmarks().context("start security-scoped bookmarks")?;
        return Ok(LaunchHold {
            child_exit: None,
            swtpm: None,
            bookmarks: Some(held),
        });
    }
    if skip_sandbox() {
        return Ok(LaunchHold {
            child_exit: None,
            swtpm: None,
            bookmarks: None,
        });
    }
    let text = std::fs::read_to_string(config_path)
        .with_context(|| format!("load vm config {}", config_path.display()))?;
    let mut config = VmConfig::parse(&text)
        .with_context(|| format!("load vm config {}", config_path.display()))?;
    crate::tools::auto_attach_windows_tools(&mut config);

    let swtpm = start_swtpm_if_needed(&config).context("launcher start swtpm")?;

    let exe = std::env::current_exe().context("current_exe for sandboxed VMM")?;
    let args = vec![
        "run".into(),
        "--display".into(),
        display_arg(mode),
        "--fps".into(),
        fps.to_string(),
        config_path.display().to_string(),
    ];
    if let Some(daemon) = &swtpm {
        // SAFETY: the launcher is still single-threaded; the Seatbelt child
        // inherits these socket paths and does not spawn swtpm itself.
        unsafe {
            std::env::set_var(SWTPM_CTRL_ENV, &daemon.ctrl_path);
            std::env::set_var(SWTPM_DATA_ENV, &daemon.data_path);
        }
    }
    let code = maybe_relaunch(config_path, &exe, &args, &config).context("launch sandboxed VMM")?;
    if code.is_some() && config.encryption {
        if let Ok(Some(state)) = config.tpm_state_path() {
            if let Err(error) = crate::crypto_cmd::wrap_tpm_state(&config.name, &state) {
                tracing::warn!(
                    target: "ternvale::crypto",
                    path = %state.display(),
                    error = %format!("{error:#}"),
                    "could not wrap TPM state after sandboxed VMM exit"
                );
            }
        }
    }
    Ok(LaunchHold {
        child_exit: code,
        swtpm,
        bookmarks: None,
    })
}

/// Debug probe of `~/.ssh`, `/etc/hosts`, and `/bin/sh`. Call after logging.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub(crate) fn run_sandbox_probe() -> Result<()> {
    let force = probe_requested();
    if !force && !is_sandboxed() {
        return Ok(());
    }
    if !force && !cfg!(debug_assertions) {
        return Ok(());
    }
    let report = run_debug_probe();
    if force && !report.all_denied() {
        let allowed: Vec<String> = report
            .items
            .iter()
            .filter(|item| !item.denied)
            .map(|item| format!("{} {} ({})", item.name, item.target, item.detail))
            .collect();
        anyhow::bail!(
            "TERNVALE_SANDBOX_PROBE: Seatbelt allowed a forbidden host access: {}",
            allowed.join("; ")
        );
    }
    Ok(())
}

fn display_arg(mode: DisplayMode) -> String {
    match mode {
        DisplayMode::None => "none".into(),
        DisplayMode::Window => "window".into(),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
fn start_swtpm_if_needed(config: &VmConfig) -> Result<Option<SwtpmDaemon>> {
    if !config.tpm_enabled() {
        return Ok(None);
    }
    let section = config.tpm_section();
    if section.backend == TpmBackendKind::Stub {
        return Ok(None);
    }
    if config.effective_firmware_tables() != FirmwareTables::Acpi {
        return Ok(None);
    }
    let Some(state) = config.tpm_state_path().context("tpm state path")? else {
        return Ok(None);
    };
    if section.backend == TpmBackendKind::Auto && find_swtpm(section.swtpm.as_deref()).is_err() {
        tracing::warn!(
            target: "ternvale::cli",
            "launcher: swtpm missing; sandboxed VMM will use the stub"
        );
        return Ok(None);
    }
    if config.encryption {
        crate::crypto_cmd::unwrap_tpm_state(&config.name, &state)
            .with_context(|| format!("unwrap TPM state {}", state.display()))?;
    }
    let daemon = SwtpmDaemon::start(section.swtpm.as_deref(), &state)
        .with_context(|| format!("spawn swtpm for {}", state.display()))?;
    Ok(Some(daemon))
}
