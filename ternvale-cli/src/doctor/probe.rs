//! Read the host for `ternvale doctor`: sysctls, the binary's signature, a
//! probe VM with a GIC, free disk space, and the log directory.

use std::path::{Path, PathBuf};
use std::process::Command;

use ternvale_hv::HvError;
use ternvale_log::LogConfig;
use ternvale_vmm::{GIC_DIST_BASE, GIC_REDIST_BASE};

use super::{DiskFacts, Facts, LogDirFacts, VmProbe};

/// Gather every fact. Never fails: a fact that cannot be read becomes an `Err`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn gather() -> Facts {
    let exe = std::env::current_exe().unwrap_or_else(|error| {
        tracing::warn!(target: "ternvale::cli", error = %error, "current_exe failed");
        PathBuf::from("ternvale")
    });
    let hv_support = ternvale_hv::hypervisor_supported().map_err(|e| e.to_string());
    let (vm, gic) = if matches!(hv_support, Ok(false)) {
        (Err(VmProbe::Other("kern.hv_support is 0".into())), None)
    } else {
        probe_vm()
    };
    let log_dir = LogConfig::default_log_dir().map_err(|e| e.to_string());
    let disk = match &log_dir {
        Ok(dir) => disk_facts(dir),
        Err(error) => Err(error.clone()),
    };
    Facts {
        macos: ternvale_hv::macos_version().map_err(|e| e.to_string()),
        hv_support,
        entitlements: entitlements(&exe),
        exe,
        vm,
        gic,
        disk,
        log_dir: log_dir.and_then(|dir| log_dir_facts(&dir)),
    }
}

/// Create a VM and a GIC in it, then tear both down.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn probe_vm() -> (Result<(), VmProbe>, Option<Result<(), String>>) {
    match ternvale_hv::Vm::create() {
        Ok(vm) => {
            let gic = vm
                .create_gic(GIC_DIST_BASE, GIC_REDIST_BASE)
                .map(drop)
                .map_err(|e| e.to_string());
            tracing::debug!(target: "ternvale::cli", gic_ok = gic.is_ok(), "probe vm created");
            drop(vm);
            (Ok(()), Some(gic))
        }
        Err(HvError::Denied { .. }) => (Err(VmProbe::Denied), None),
        Err(error) => (Err(VmProbe::Other(error.to_string())), None),
    }
}

/// `codesign -d --entitlements - --xml <exe>` stdout. An unsigned binary
/// yields an empty string (it has no entitlements), not an error.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(exe = %exe.display()))]
pub fn entitlements(exe: &Path) -> Result<String, String> {
    let output = Command::new("/usr/bin/codesign")
        .args(["-d", "--entitlements", "-", "--xml"])
        .arg(exe)
        .output()
        .map_err(|error| format!("run /usr/bin/codesign: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    tracing::debug!(target: "ternvale::cli", status = ?output.status.code(), stdout_bytes = stdout.len(), stderr = %stderr.trim(), "codesign -d");
    if output.status.success() || stderr.contains("not signed") {
        Ok(stdout)
    } else {
        Err(stderr.trim().to_string())
    }
}

/// Free and total bytes on the volume holding `path` (or its nearest
/// existing ancestor, so a log directory not created yet still works).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn disk_facts(path: &Path) -> Result<DiskFacts, String> {
    let existing = path
        .ancestors()
        .find(|p| p.exists())
        .ok_or_else(|| format!("no existing ancestor of {}", path.display()))?;
    // statfs, not statvfs: Darwin's statvfs has 32-bit block counts.
    let stat = nix::sys::statfs::statfs(existing)
        .map_err(|errno| format!("statfs {}: {errno}", existing.display()))?;
    let block = u64::from(stat.block_size());
    let facts = DiskFacts {
        path: path.to_path_buf(),
        free_bytes: stat.blocks_available().saturating_mul(block),
        total_bytes: stat.blocks().saturating_mul(block),
    };
    tracing::debug!(target: "ternvale::cli", free = facts.free_bytes, total = facts.total_bytes, "disk space");
    Ok(facts)
}

/// Create `dir`, write and remove a probe file, and total its contents.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(dir = %dir.display()))]
pub fn log_dir_facts(dir: &Path) -> Result<LogDirFacts, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let probe = dir.join(format!(".doctor-probe-{}", std::process::id()));
    std::fs::write(&probe, b"ternvale doctor\n")
        .map_err(|e| format!("{} is not writable: {e}", dir.display()))?;
    std::fs::remove_file(&probe).map_err(|e| format!("remove {}: {e}", probe.display()))?;
    let mut files = 0;
    let mut bytes = 0;
    let entries = std::fs::read_dir(dir).map_err(|e| format!("list {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("list {}: {e}", dir.display()))?;
        match entry.metadata() {
            Ok(meta) if meta.is_file() => {
                files += 1;
                bytes += meta.len();
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(target: "ternvale::cli", path = %entry.path().display(), error = %error, "could not stat log file")
            }
        }
    }
    Ok(LogDirFacts {
        path: dir.to_path_buf(),
        files,
        bytes,
    })
}
