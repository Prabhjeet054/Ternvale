//! `ternvale create --macos`: restore image, platform identity, VZ installer.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use ternvale_api::{Event, EventKind};
use ternvale_backend::{self as backend, AUX_NAME, IDENTITY_NAME};
use ternvale_config::VmConfig;

use crate::disk;
use crate::doctor;
use crate::paths;

/// Overrides measured free space so tests can refuse create before a download.
pub const MOCK_FREE_BYTES_ENV: &str = "TERNVALE_MOCK_FREE_BYTES";

/// Default system disk for a macOS guest.
pub const DEFAULT_DISK: &str = "64G";
/// Default vCPUs (raised to the restore image minimum if that is higher).
/// 2 stays at Apple's `minimumSupportedCPUCount` and fits an 16–18 GiB host.
pub const DEFAULT_CPUS: u32 = 2;
/// Default RAM in MiB. 4096 is Apple's restore-image minimum; 8192 OOMs 18 GiB hosts.
pub const DEFAULT_RAM_MIB: u64 = 4096;

/// Flags after clap parse.
pub struct CreateMacos {
    pub name: String,
    pub ipsw: Option<PathBuf>,
    pub sha256: Option<String>,
    pub disk_size: String,
    pub cpus: u32,
    pub ram_mib: u64,
    pub wait: bool,
}

/// `ternvale create --macos`.
#[tracing::instrument(level = "info", target = "ternvale::cli", skip_all, fields(vm = %args.name, wait = args.wait))]
pub fn create_macos(args: CreateMacos) -> Result<ExitCode> {
    paths::socket_path(&args.name)?;
    let dest = paths::vm_dir(&args.name)?;
    if dest.exists() {
        bail!("VM {:?} already exists at {}", args.name, dest.display());
    }
    tracing::info!(
        target: "ternvale::cli",
        vm = %args.name,
        ipsw = ?args.ipsw,
        "macos create: inspect restore image"
    );
    let info = backend::inspect_restore(args.ipsw.as_deref()).map_err(anyhow::Error::msg)?;
    backend::require_supported(&info).map_err(anyhow::Error::msg)?;
    let cpus = args.cpus.max(info.min_cpus.max(1));
    let ram_bytes = args
        .ram_mib
        .saturating_mul(1024)
        .saturating_mul(1024)
        .max(info.min_ram_bytes);
    let disk_bytes = disk::parse_size(&args.disk_size)?;
    let need = doctor::macos_create_need(disk_bytes, info.restore_bytes());
    let free = free_bytes_for_create(&dest);
    if let Err(message) = doctor::require_macos_create_space(free, need) {
        bail!("{message}");
    }

    fs::create_dir_all(dest.join("disks")).context("create VM disks directory")?;
    let disk_path = dest.join("disks/system.img");
    disk::create_disk(&disk_path, disk_bytes)?;

    let ipsw_path = if let Some(path) = args.ipsw.as_ref() {
        path.canonicalize()
            .with_context(|| format!("IPSW {}", path.display()))?
    } else {
        let cache = paths::ipsw_cache_dir()?;
        fs::create_dir_all(&cache).context("create IPSW cache")?;
        let name = if info.build.is_empty() {
            "latest.ipsw".into()
        } else {
            format!("{}.ipsw", info.build)
        };
        let dest_ipsw = cache.join(name);
        let last = Arc::new(AtomicU32::new(0));
        let last2 = Arc::clone(&last);
        let downloaded = backend::download_restore(
            &info.url,
            &dest_ipsw,
            args.sha256.as_deref(),
            move |completed, total, permille| {
                report_progress("download", &last2, completed, total, permille);
            },
        )
        .map_err(anyhow::Error::msg)?;
        if !downloaded.sha256.is_empty() {
            let sidecar = dest_ipsw.with_extension("ipsw.sha256");
            fs::write(&sidecar, format!("{}\n", downloaded.sha256))
                .with_context(|| format!("write {}", sidecar.display()))?;
        }
        println!(
            "progress: download complete sha256={} bytes={} resumed={}",
            downloaded.sha256, downloaded.bytes, downloaded.resumed
        );
        dest_ipsw
    };

    let identity = backend::persist_platform(&dest, &info).map_err(anyhow::Error::msg)?;
    let aux = dest.join(AUX_NAME);
    if std::env::var_os("TERNVALE_SKIP_MACOS_INSTALL").is_none() {
        let last = Arc::new(AtomicU32::new(0));
        backend::install_macos(
            &ipsw_path,
            &disk_path,
            &identity,
            &aux,
            cpus,
            ram_bytes,
            move |completed, total, permille| {
                report_progress("install", &last, completed, total, permille);
            },
        )
        .map_err(anyhow::Error::msg)?;
    } else {
        tracing::info!(target: "ternvale::cli", "TERNVALE_SKIP_MACOS_INSTALL set; not running VZMacOSInstaller");
    }

    let ram_mib = ram_bytes / 1024 / 1024;
    let config = macos_config(&args.name, &dest, &disk_path, cpus, ram_mib)?;
    config
        .validate()
        .context("validate generated macOS config")?;
    let config_path = paths::vm_config_path(&args.name)?;
    fs::write(
        &config_path,
        config.to_toml().context("serialize VM config")?,
    )
    .with_context(|| format!("write {}", config_path.display()))?;
    tracing::info!(
        target: "ternvale::cli",
        vm = %args.name,
        config = %config_path.display(),
        identity = IDENTITY_NAME,
        "created macOS VM"
    );
    println!("created {}", args.name);
    println!("  config: {}", config_path.display());
    println!("  disk: {}", disk_path.display());
    println!("  ipsw: {}", ipsw_path.display());
    println!("  platform: {}", dest.join(IDENTITY_NAME).display());
    println!("  hardware_model: {}", identity.hardware_hex());
    println!("  machine_identifier: {}", identity.identifier_hex());
    println!("  see docs/MACOS_GUESTS.md for Apple software licence limits");
    println!("start with: ternvale run {}", config_path.display());
    Ok(ExitCode::SUCCESS)
}

fn macos_config(name: &str, dest: &Path, disk: &Path, cpus: u32, ram_mib: u64) -> Result<VmConfig> {
    VmConfig::parse(&format!(
        "name = {name}\nprofile = \"macos\"\nbackend = \"vz\"\ncpus = {cpus}\nram_mib = {ram_mib}\nserial_log = {serial}\n\n[[disks]]\npath = {disk}\nread_only = false\n\n[[nics]]\nbackend = \"user\"\n\n[vsock]\nagent = true\n\n[tpm]\nenabled = false\n",
        name = toml_string(name),
        serial = toml_string(&dest.join("serial.log").display().to_string()),
        disk = toml_string(&disk.display().to_string()),
    ))
    .context("parse generated macOS config")
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Measured free bytes, or [`MOCK_FREE_BYTES_ENV`] when set (tests).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(dest = %dest.display()))]
pub fn free_bytes_for_create(dest: &Path) -> u64 {
    if let Ok(raw) = std::env::var(MOCK_FREE_BYTES_ENV) {
        match raw.parse::<u64>() {
            Ok(n) => {
                tracing::warn!(
                    target: "ternvale::cli",
                    mock_free_bytes = n,
                    "TERNVALE_MOCK_FREE_BYTES overrides measured free space"
                );
                return n;
            }
            Err(error) => {
                tracing::warn!(
                    target: "ternvale::cli",
                    value = %raw,
                    error = %error,
                    "ignoring unparsable TERNVALE_MOCK_FREE_BYTES"
                );
            }
        }
    }
    match crate::doctor::probe::disk_facts(dest) {
        Ok(facts) => facts.free_bytes,
        Err(_) => crate::doctor::probe::disk_facts(&std::env::temp_dir())
            .map(|f| f.free_bytes)
            .unwrap_or(0),
    }
}

/// Keep installer/download progress monotonic. `false` = skip this sample.
#[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
pub fn accept_progress(last: &AtomicU32, permille: u32) -> bool {
    let prev = last.load(Ordering::Relaxed);
    if permille < prev {
        tracing::warn!(
            target: "ternvale::cli",
            prev,
            permille,
            "macos progress went backwards; ignoring"
        );
        return false;
    }
    if permille == prev && permille != 0 && permille != 1000 {
        return false;
    }
    last.store(permille, Ordering::Relaxed);
    true
}

fn report_progress(stage: &str, last: &AtomicU32, completed: u64, total: u64, permille: u32) {
    if !accept_progress(last, permille) {
        return;
    }
    let event = Event {
        kind: EventKind::InstallProgress,
        vm: String::new(),
        state: Some(stage.into()),
        tools_outdated: None,
        line: None,
        completed: Some(completed),
        total: Some(total),
        permille: Some(permille),
    };
    tracing::info!(
        target: "ternvale::cli",
        kind = event.kind.as_str(),
        stage,
        completed,
        total,
        permille,
        "macos progress (install-progress event)"
    );
    println!("progress: {stage} {permille}‰ ({completed}/{total})");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_macos_toml_uses_vz_and_disables_tpm() {
        let text = format!(
            "name = {name}\nprofile = \"macos\"\nbackend = \"vz\"\n",
            name = toml_string("guest")
        );
        assert!(text.contains("profile = \"macos\""));
        assert!(text.contains("backend = \"vz\""));
        assert_eq!(toml_string(r#"a"b"#), r#""a\"b""#);
    }

    #[test]
    fn progress_samples_are_monotonic() {
        let last = AtomicU32::new(0);
        assert!(accept_progress(&last, 0));
        assert!(accept_progress(&last, 10));
        assert!(!accept_progress(&last, 5), "backwards");
        assert_eq!(last.load(Ordering::Relaxed), 10);
        assert!(!accept_progress(&last, 10), "duplicate");
        assert!(accept_progress(&last, 1000));
        assert!(accept_progress(&last, 1000), "terminal duplicate allowed");
    }

    #[test]
    fn mock_free_bytes_env_is_honoured() {
        let prev = std::env::var_os(MOCK_FREE_BYTES_ENV);
        std::env::set_var(MOCK_FREE_BYTES_ENV, "1234");
        let n = free_bytes_for_create(Path::new("/"));
        match prev {
            Some(v) => std::env::set_var(MOCK_FREE_BYTES_ENV, v),
            None => std::env::remove_var(MOCK_FREE_BYTES_ENV),
        }
        assert_eq!(n, 1234);
    }
}
