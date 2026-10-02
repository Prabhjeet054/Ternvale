//! Crash-report inputs written by `ternvale run` beside its host log.
//!
//! For a host log `ternvale-<vm>-<stamp>.log` the sidecars share the stem:
//! `.run.json` (written at start: config path, serial log, pid), and
//! `.dtb`, `.mmio.txt`, `.summary.txt` (written when the VM stops, and on a
//! `dump-diagnostics` control request while it runs).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use ternvale_devices::{AgentServer, BlkStats, NetStats, VsockStats};
use ternvale_vmm::{lockwatch, VmControl};

use crate::summary::{self, DiskSummary, Exit, NicSummary, VmSummary};

/// Sidecar suffixes, after the log stem.
pub const RUN_SUFFIX: &str = ".run.json";
pub const DTB_SUFFIX: &str = ".dtb";
pub const MMIO_SUFFIX: &str = ".mmio.txt";
pub const SUMMARY_SUFFIX: &str = ".summary.txt";

/// Device counters gathered while devices are attached.
#[derive(Default)]
pub struct DeviceStats {
    pub disks: Vec<(PathBuf, Arc<BlkStats>)>,
    pub nics: Vec<(String, Arc<NetStats>)>,
    pub vsock: Option<Arc<VsockStats>>,
}

/// Shared between the attach closure and the [`DiagSink`].
pub type SharedDevices = Arc<Mutex<DeviceStats>>;

/// What `.run.json` records about one `ternvale run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunManifest {
    pub name: String,
    pub pid: u32,
    pub version: String,
    pub config: PathBuf,
    pub serial_log: PathBuf,
    pub host_log: PathBuf,
    pub started: String,
}

/// `path` with `suffix` appended to its log stem (`x.log` → `x<suffix>`).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn sidecar(log: &Path, suffix: &str) -> PathBuf {
    let stem = log.with_extension("");
    let mut name = stem.into_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Write `.run.json` beside `manifest.host_log`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = %manifest.name))]
pub fn write_manifest(manifest: &RunManifest) -> Result<PathBuf> {
    let path = sidecar(&manifest.host_log, RUN_SUFFIX);
    let json = serde_json::to_string_pretty(manifest).context("encode run manifest")?;
    std::fs::write(&path, json + "\n")
        .with_context(|| format!("write run manifest {}", path.display()))?;
    tracing::debug!(target: "ternvale::cli", path = %path.display(), "run manifest written");
    Ok(path)
}

/// Read a `.run.json`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn read_manifest(path: &Path) -> Result<RunManifest> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read run manifest {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse run manifest {}", path.display()))
}

/// Builds summaries and writes the stop-time sidecars for one VM.
pub struct DiagSink {
    log: PathBuf,
    control: Arc<VmControl>,
    agent: Option<Arc<AgentServer>>,
    devices: SharedDevices,
}

impl DiagSink {
    /// Sidecars go beside `log`.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = control.name()))]
    pub fn new(
        log: &Path,
        control: Arc<VmControl>,
        agent: Option<Arc<AgentServer>>,
        devices: SharedDevices,
    ) -> Self {
        Self {
            log: log.to_path_buf(),
            control,
            agent,
            devices,
        }
    }

    /// The VM's counters now; `exit` is how the run ended, if it has.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = self.control.name()))]
    pub fn summary(&self, exit: Option<Exit>) -> VmSummary {
        let mmio = self.control.diagnostics().mmio();
        let devices = lockwatch::lock(&self.devices, "device-stats");
        VmSummary {
            exit,
            stats: self.control.stats(),
            mmio_total: mmio.total(),
            mmio_unmapped: mmio.unmapped(),
            mmio: mmio.devices(),
            disks: devices
                .disks
                .iter()
                .map(|(path, stats)| {
                    let (reqs, bytes, errors) = stats.snapshot();
                    DiskSummary {
                        path: path.clone(),
                        reqs,
                        bytes,
                        errors,
                    }
                })
                .collect(),
            nics: devices
                .nics
                .iter()
                .map(|(backend, stats)| NicSummary {
                    backend: backend.clone(),
                    counters: stats.snapshot(),
                })
                .collect(),
            vsock: devices.vsock.as_ref().map(|stats| stats.snapshot()),
            agent: self.agent.as_ref().map(|agent| agent.status()),
        }
    }

    /// Write `.dtb` (once the machine built one), `.mmio.txt`, and
    /// `.summary.txt`; returns the files written.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = self.control.name(), stopped = exit.is_some()))]
    pub fn dump(&self, exit: Option<Exit>) -> Result<Vec<PathBuf>> {
        let diag = self.control.diagnostics();
        let mut written = Vec::new();
        if let Some(dtb) = diag.dtb() {
            let path = sidecar(&self.log, DTB_SUFFIX);
            std::fs::write(&path, dtb)
                .with_context(|| format!("write guest dtb {}", path.display()))?;
            written.push(path);
        }
        let mmio = sidecar(&self.log, MMIO_SUFFIX);
        std::fs::write(&mmio, render_mmio(&self.control))
            .with_context(|| format!("write mmio events {}", mmio.display()))?;
        written.push(mmio);
        let text = summary::render(&self.summary(exit));
        let path = sidecar(&self.log, SUMMARY_SUFFIX);
        std::fs::write(&path, text + "\n")
            .with_context(|| format!("write vm summary {}", path.display()))?;
        written.push(path);
        tracing::info!(target: "ternvale::cli", files = written.len(), dir = %self.log.parent().unwrap_or(Path::new("")).display(), "diagnostics written");
        Ok(written)
    }
}

/// The MMIO ring and per-window counts as text, oldest event first.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = control.name()))]
pub fn render_mmio(control: &VmControl) -> String {
    let trace = control.diagnostics().mmio();
    let events = trace.events();
    let mut lines = vec![
        format!(
            "# vm {}: {} mmio accesses, {} unmapped, {} not kept (ring slot race); last {} below",
            control.name(),
            trace.total(),
            trace.unmapped(),
            trace.dropped(),
            events.len()
        ),
        "# windows: name base size accesses".to_string(),
    ];
    lines.extend(trace.devices().into_iter().map(|device| {
        format!(
            "window {} {:#x} {:#x} {}",
            device.name, device.base, device.size, device.accesses
        )
    }));
    lines.push("# seq time_us cpu dir gpa size value device+offset".to_string());
    lines.extend(events.into_iter().map(|e| {
        let cpu = e.cpu.map_or_else(|| "-".to_string(), |cpu| cpu.to_string());
        format!(
            "{} {} {} {} {:#x} {} {:#x} {}+{:#x}",
            e.seq,
            e.at_us,
            cpu,
            if e.write { "W" } else { "R" },
            e.gpa,
            e.size,
            e.value,
            e.device,
            e.offset
        )
    }));
    lines.join("\n") + "\n"
}

#[cfg(test)]
#[path = "diag_tests.rs"]
mod tests;
