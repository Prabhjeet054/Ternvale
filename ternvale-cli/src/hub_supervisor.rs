//! Hub-side VMM process supervisor (`ternvale serve`).
//!
//! Restarts a VM whose `ternvale run` child died, with exponential backoff,
//! an unclean-shutdown marker, and a stop after 3 crashes in 5 minutes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result};
use ternvale_config::{OnVmmCrash, VmConfig};
use ternvale_vmm::crash::mark_unclean_shutdown;
use ternvale_vmm::{lockwatch, SuperviseDecision, VmmSupervisor};

struct Slot {
    wanted: bool,
    supervisor: VmmSupervisor,
    origin: Instant,
}

fn slots() -> &'static Mutex<HashMap<String, Slot>> {
    static SLOTS: OnceLock<Mutex<HashMap<String, Slot>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Spawn `ternvale run` and watch the child. Unexpected exits follow
/// `[reliability].on_vmm_crash`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::watchdog",
    skip_all,
    fields(vm = name, config = %config.display())
)]
pub fn start_supervised(name: &str, bin: &Path, config: &Path) -> Result<()> {
    {
        let mut map = lockwatch::lock(slots(), "hub-supervisor");
        let slot = map.entry(name.to_string()).or_insert_with(|| Slot {
            wanted: true,
            supervisor: VmmSupervisor::new(),
            origin: Instant::now(),
        });
        slot.wanted = true;
    }
    spawn_child(name.to_string(), bin.to_path_buf(), config.to_path_buf())?;
    Ok(())
}

/// A hub `stop` (or graceful shutdown) so an exit is not treated as a crash.
#[tracing::instrument(level = "debug", target = "ternvale::watchdog", skip_all, fields(vm = name))]
pub fn note_user_stop(name: &str) {
    let mut map = lockwatch::lock(slots(), "hub-supervisor");
    if let Some(slot) = map.get_mut(name) {
        slot.wanted = false;
        tracing::info!(
            target: "ternvale::watchdog",
            vm = name,
            "hub stop: supervisor will not restart this VM"
        );
    }
}

fn spawn_child(name: String, bin: PathBuf, config: PathBuf) -> Result<()> {
    let child = Command::new(&bin)
        .arg("run")
        .arg(&config)
        .spawn()
        .with_context(|| format!("spawn {} run {}", bin.display(), config.display()))?;
    tracing::info!(
        target: "ternvale::watchdog",
        vm = %name,
        pid = child.id(),
        "hub started VMM process"
    );
    thread::Builder::new()
        .name(format!("vmm-super-{name}"))
        .spawn(move || watch(name, bin, config, child))
        .context("spawn hub VMM supervisor thread")?;
    Ok(())
}

fn watch(name: String, bin: PathBuf, config: PathBuf, mut child: Child) {
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            tracing::error!(
                target: "ternvale::watchdog",
                vm = %name,
                error = %error,
                reason = "wait on VMM process failed",
                "hub supervisor lost the VMM child"
            );
            return;
        }
    };
    let wanted = {
        let map = lockwatch::lock(slots(), "hub-supervisor");
        map.get(&name).is_some_and(|s| s.wanted)
    };
    if !wanted {
        tracing::info!(
            target: "ternvale::watchdog",
            vm = %name,
            ?status,
            "VMM process exited after a requested stop"
        );
        return;
    }
    let reason = format!("VMM process died unexpectedly ({status})");
    tracing::error!(
        target: "ternvale::watchdog",
        vm = %name,
        ?status,
        reason = %reason,
        "hub supervisor saw an unexpected VMM exit"
    );
    let dir = config.parent().unwrap_or(Path::new("."));
    if let Err(error) = mark_unclean_shutdown(dir, &reason) {
        tracing::error!(
            target: "ternvale::watchdog",
            vm = %name,
            error = %error,
            "failed to write unclean-shutdown marker"
        );
    }
    let vm = match VmConfig::from_path(&config) {
        Ok(vm) => vm,
        Err(error) => {
            tracing::error!(
                target: "ternvale::watchdog",
                vm = %name,
                error = %error,
                "could not reload config after VMM crash; not restarting"
            );
            return;
        }
    };
    if vm.reliability.on_vmm_crash == OnVmmCrash::Stop {
        tracing::error!(
            target: "ternvale::watchdog",
            vm = %name,
            policy = "stop",
            reason = "on_vmm_crash=stop",
            "hub supervisor will not restart"
        );
        return;
    }
    let decision = {
        let mut map = lockwatch::lock(slots(), "hub-supervisor");
        let Some(slot) = map.get_mut(&name) else {
            return;
        };
        if !slot.wanted {
            return;
        }
        slot.supervisor.note_crash(slot.origin.elapsed())
    };
    match decision {
        SuperviseDecision::Restart { backoff, crash_n } => {
            tracing::error!(
                target: "ternvale::watchdog",
                vm = %name,
                crash_n,
                backoff_ms = backoff.as_millis() as u64,
                reason = "vmm process died unexpectedly",
                "hub supervisor restarting the VM"
            );
            thread::sleep(backoff);
            let still = {
                let map = lockwatch::lock(slots(), "hub-supervisor");
                map.get(&name).is_some_and(|s| s.wanted)
            };
            if !still {
                return;
            }
            if let Err(error) = spawn_child(name.clone(), bin, config) {
                tracing::error!(
                    target: "ternvale::watchdog",
                    vm = %name,
                    error = %format!("{error:#}"),
                    reason = "spawn after crash failed",
                    "hub supervisor could not restart the VMM"
                );
            }
        }
        SuperviseDecision::Stop { reason, crashes } => {
            tracing::error!(
                target: "ternvale::watchdog",
                vm = %name,
                crashes,
                reason,
                "hub supervisor stopping restarts"
            );
            let mut map = lockwatch::lock(slots(), "hub-supervisor");
            if let Some(slot) = map.get_mut(&name) {
                slot.wanted = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_stop_clears_wanted() {
        let name = format!("hub-super-{}", std::process::id());
        {
            let mut map = lockwatch::lock(slots(), "hub-supervisor");
            map.insert(
                name.clone(),
                Slot {
                    wanted: true,
                    supervisor: VmmSupervisor::new(),
                    origin: Instant::now(),
                },
            );
        }
        note_user_stop(&name);
        let map = lockwatch::lock(slots(), "hub-supervisor");
        assert!(!map.get(&name).expect("slot").wanted);
    }
}
