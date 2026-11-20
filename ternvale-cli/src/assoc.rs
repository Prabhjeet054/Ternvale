//! First-use association approvals and guest → host `open`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use ternvale_config::{
    check_host_url, file_ext, guest_file_to_host, is_blocked_exec_ext, AgentSection, Share,
};

/// Magic `TVAP` (Ternvale association approvals).
pub const MAGIC: [u8; 4] = *b"TVAP";
/// Current on-disk version.
pub const VERSION: u32 = 1;

const OPEN_BIN: &str = "/usr/bin/open";
/// Override `/usr/bin/open` (quality tests; never a guest-supplied path).
pub const OPEN_BIN_ENV: &str = "TERNVALE_OPEN_BIN";

/// Persisted first-use keys (`guest-app:…`, `guest-open:…`, `host-url:…`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Store {
    keys: BTreeSet<String>,
}

/// Path of the versioned approvals file for `vm`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm))]
pub fn approvals_path(vm: &str) -> Result<PathBuf> {
    Ok(crate::paths::vm_dir(vm)?.join("assoc-approvals.tvap"))
}

/// Load keys; missing file is empty. Unknown versions are errors.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn load(path: &Path) -> Result<BTreeSet<String>> {
    if !path.is_file() {
        return Ok(BTreeSet::new());
    }
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if bytes.len() < 8 || bytes[0..4] != MAGIC {
        bail!(
            "approvals {} is not a Ternvale association file",
            path.display()
        );
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes"));
    if version != VERSION {
        bail!(
            "approvals {} version {version} is unsupported (this build speaks {VERSION})",
            path.display()
        );
    }
    let store: Store = serde_json::from_slice(&bytes[8..])
        .with_context(|| format!("parse approvals {}", path.display()))?;
    Ok(store.keys)
}

/// Persist keys.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display(), count = keys.len()))]
pub fn save(path: &Path, keys: &BTreeSet<String>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let json = serde_json::to_vec(&Store { keys: keys.clone() }).context("encode approvals")?;
    let mut bytes = Vec::with_capacity(8 + json.len());
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&json);
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
    tracing::info!(target: "ternvale::cli", path = %path.display(), count = keys.len(), "wrote association approvals");
    Ok(())
}

/// Add one approval key.
#[tracing::instrument(level = "info", target = "ternvale::cli", skip_all, fields(vm, key))]
pub fn approve(vm: &str, key: &str) -> Result<()> {
    if key.is_empty() || key.contains('\0') {
        bail!("approval key is empty");
    }
    let path = approvals_path(vm)?;
    let mut keys = load(&path)?;
    keys.insert(key.to_string());
    save(&path, &keys)?;
    Ok(())
}

/// Whether `key` was confirmed.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, key))]
pub fn is_approved(vm: &str, key: &str) -> Result<bool> {
    Ok(load(&approvals_path(vm)?)?.contains(key))
}

/// First-use gate: `Ok(true)` needs a confirmation (`--confirm` / `apps.approve`).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(vm, key, confirm)
)]
pub fn needs_confirmation(vm: &str, key: &str, confirm: bool) -> Result<bool> {
    if is_approved(vm, key)? {
        return Ok(false);
    }
    if confirm {
        approve(vm, key)?;
        return Ok(false);
    }
    tracing::info!(target: "ternvale::cli", vm, key, "association needs first-use confirmation");
    Ok(true)
}

/// Guest → host open (never auto-executes a file).
#[tracing::instrument(level = "info", target = "ternvale::cli", skip_all, fields(vm, kind))]
pub fn handle_guest_open(
    vm: &str,
    policy: &AgentSection,
    shares: &[Share],
    kind: &str,
    target: &str,
) -> Result<(), String> {
    let result = match kind {
        "url" => open_url(vm, policy, target, false),
        "file" => open_guest_file(vm, policy, shares, target, false),
        _ => Err(format!("host_open unknown kind {kind}")),
    };
    if let Err(reason) = &result {
        tracing::warn!(
            target: "ternvale::cli",
            vm,
            kind,
            reason = %reason,
            "guest host_open blocked"
        );
    } else {
        tracing::info!(
            target: "ternvale::cli",
            vm,
            kind,
            "guest host_open handed to macOS"
        );
    }
    result
}

fn open_url(vm: &str, policy: &AgentSection, url: &str, confirm: bool) -> Result<(), String> {
    let scheme = check_host_url(policy, url)?;
    let key = format!("host-url:{scheme}");
    if needs_confirmation(vm, &key, confirm).map_err(|e| e.to_string())? {
        return Err(format!(
            "needs confirmation for first use of {key} (ternvale apps approve {vm} --key {key})"
        ));
    }
    run_open(&["-u", url])
}

fn open_guest_file(
    vm: &str,
    policy: &AgentSection,
    shares: &[Share],
    guest: &str,
    confirm: bool,
) -> Result<(), String> {
    if !policy.open_on_host {
        return Err("guest → host open is disabled (set agent.open_on_host = true)".into());
    }
    let host = guest_file_to_host(shares, guest)?;
    let ext = file_ext(&host);
    if is_blocked_exec_ext(&ext) {
        tracing::warn!(
            target: "ternvale::cli",
            ext = %ext,
            "refusing to open an executable-like file on the host"
        );
        return Err(format!("refusing to auto-execute .{ext} files on the host"));
    }
    let key = format!("host-file:{ext}");
    if needs_confirmation(vm, &key, confirm).map_err(|e| e.to_string())? {
        return Err(format!(
            "needs confirmation for first use of {key} (ternvale apps approve {vm} --key {key})"
        ));
    }
    run_open(&[host.to_str().ok_or("host path is not UTF-8")?])
}

fn run_open(args: &[&str]) -> Result<(), String> {
    let bin = std::env::var_os(OPEN_BIN_ENV)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(OPEN_BIN));
    tracing::debug!(
        target: "ternvale::cli",
        bin = %bin.display(),
        argc = args.len(),
        "host open"
    );
    let status = std::process::Command::new(&bin)
        .args(args)
        .status()
        .map_err(|error| format!("open: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("open exited {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_versioned_store_and_rejects_unknown_version() {
        let dir = std::env::temp_dir().join(format!("tv-appr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("assoc-approvals.tvap");
        let mut keys = BTreeSet::new();
        keys.insert("host-url:https".into());
        save(&path, &keys).expect("save");
        let loaded = load(&path).expect("load");
        assert!(loaded.contains("host-url:https"));
        let mut bad = std::fs::read(&path).expect("read");
        bad[4..8].copy_from_slice(&99u32.to_le_bytes());
        std::fs::write(&path, bad).expect("write");
        let err = load(&path).expect_err("version");
        assert!(err.to_string().contains("unsupported"), "{err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
