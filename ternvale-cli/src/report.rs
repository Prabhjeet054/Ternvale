//! `ternvale report <name>`: zip everything needed to debug a VM run.
//!
//! The bundle holds the newest host log, the VM config, the guest serial
//! log, the guest DTB (and a DTS when `dtc` is installed), the last MMIO
//! events, the stop summary, the run manifest, and a `doctor` report. A
//! running VM is first asked to `dump-diagnostics`; for a VM that exited,
//! the files `ternvale run` wrote at stop are used.

pub mod bundle;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{bail, Context, Result};
use ternvale_log::LogConfig;

use self::bundle::Item;
use crate::diag::{self, DTB_SUFFIX, MMIO_SUFFIX, RUN_SUFFIX, SUMMARY_SUFFIX};
use crate::protocol::Request;

/// Largest host log tail in a report.
pub const HOST_LOG_CAP: u64 = 32 << 20;
/// Largest guest serial log tail in a report.
pub const SERIAL_LOG_CAP: u64 = 8 << 20;
/// Largest config or sidecar file copied.
pub const SMALL_CAP: u64 = 4 << 20;

/// How the report got the running VM's diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Live {
    /// The VM answered `dump-diagnostics`.
    Dumped(Vec<String>),
    /// No VM is serving the control socket.
    NotRunning,
    /// The VM is running but the dump failed.
    Failed(String),
}

/// Inputs to [`plan`] besides the log directory.
#[derive(Debug, Clone)]
pub struct PlanInputs<'a> {
    pub name: &'a str,
    pub config: Option<&'a Path>,
    pub live: Live,
    pub doctor: Option<String>,
}

/// The header lines and items of one report.
#[derive(Debug, Clone)]
pub struct Plan {
    pub top: String,
    pub header: Vec<String>,
    pub items: Vec<Item>,
}

/// Decide what goes in the report for the newest run of `inputs.name` in `dir`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(dir = %dir.display(), vm = inputs.name))]
pub fn plan(dir: &Path, inputs: &PlanInputs<'_>, stamp: &str) -> Result<Plan> {
    let name = inputs.name;
    let Some(host_log) = crate::logs::newest(dir, name)? else {
        let Some(config) = inputs.config else {
            bail!(
                "no host logs for vm {name:?} in {}; nothing to report (pass --config <vm.toml> to bundle the config and doctor's checks of it)",
                dir.display()
            );
        };
        return Ok(plan_without_run(dir, inputs, config, stamp));
    };
    let manifest_path = diag::sidecar(&host_log, RUN_SUFFIX);
    let manifest = diag::read_manifest(&manifest_path);
    if let Err(error) = &manifest {
        tracing::warn!(target: "ternvale::cli", error = %format!("{error:#}"), "no run manifest");
    }
    let manifest = manifest.ok();
    let config = inputs
        .config
        .map(Path::to_path_buf)
        .or_else(|| manifest.as_ref().map(|m| m.config.clone()));
    let serial = manifest
        .as_ref()
        .map(|m| m.serial_log.clone())
        .or_else(|| serial_log_of(config.as_deref()?));

    let mut header = vec![
        format!("Ternvale crash report for vm {name}"),
        format!(
            "generated {stamp} by ternvale {}",
            env!("CARGO_PKG_VERSION")
        ),
        format!("host log {}", host_log.display()),
    ];
    header.push(match &inputs.live {
        Live::Dumped(files) => format!(
            "vm was running: dump-diagnostics wrote {} files",
            files.len()
        ),
        Live::NotRunning => {
            "vm was not running: diagnostics are the ones written when it stopped".to_string()
        }
        Live::Failed(error) => format!(
            "vm was running but dump-diagnostics failed ({error}); diagnostics may be stale"
        ),
    });
    header.extend(doctor_failures(inputs));

    let mut items = vec![Item::file("host.log", &host_log, HOST_LOG_CAP)];
    items.push(match &config {
        Some(path) => Item::file("config.toml", path, SMALL_CAP),
        None => Item::missing("config.toml", "no run manifest; pass --config <path>"),
    });
    items.push(match &serial {
        Some(path) => Item::file("guest-serial.log", path, SERIAL_LOG_CAP),
        None => Item::missing(
            "guest-serial.log",
            "serial log path unknown (no manifest or config)",
        ),
    });
    let dtb = diag::sidecar(&host_log, DTB_SUFFIX);
    if dtb.exists() {
        items.push(Item::file("guest.dtb", &dtb, SMALL_CAP));
        items.push(match dts(&dtb) {
            Ok(text) => Item::bytes("guest.dts", text),
            Err(why) => Item::missing("guest.dts", why),
        });
    } else {
        items.push(sidecar_missing("guest.dtb", &dtb));
    }
    for (entry, suffix) in [
        ("mmio-events.txt", MMIO_SUFFIX),
        ("summary.txt", SUMMARY_SUFFIX),
        ("run.json", RUN_SUFFIX),
    ] {
        let path = diag::sidecar(&host_log, suffix);
        items.push(if path.exists() {
            Item::file(entry, &path, SMALL_CAP)
        } else {
            sidecar_missing(entry, &path)
        });
    }
    items.push(match &inputs.doctor {
        Some(text) => Item::bytes("doctor.txt", text.clone() + "\n"),
        None => Item::missing("doctor.txt", "not run"),
    });
    Ok(Plan {
        top: format!("ternvale-report-{name}-{stamp}"),
        header,
        items,
    })
}

/// A VM that never logged (its config failed before `run` set up logging):
/// the config, its serial log if any, and doctor's checks of the config.
fn plan_without_run(dir: &Path, inputs: &PlanInputs<'_>, config: &Path, stamp: &str) -> Plan {
    let name = inputs.name;
    tracing::warn!(target: "ternvale::cli", vm = name, config = %config.display(), "no host log; reporting the config only");
    let mut header = vec![
        format!("Ternvale crash report for vm {name}"),
        format!(
            "generated {stamp} by ternvale {}",
            env!("CARGO_PKG_VERSION")
        ),
        format!(
            "no host log for vm {name} in {}: `ternvale run` stops before logging when the config is invalid; doctor.txt checks the config",
            dir.display()
        ),
    ];
    header.extend(doctor_failures(inputs));
    let mut items = vec![
        Item::missing("host.log", "the vm never started logging"),
        Item::file("config.toml", config, SMALL_CAP),
    ];
    if let Some(serial) = serial_log_of(config) {
        items.push(Item::file("guest-serial.log", &serial, SERIAL_LOG_CAP));
    }
    items.push(match &inputs.doctor {
        Some(text) => Item::bytes("doctor.txt", text.clone() + "\n"),
        None => Item::missing("doctor.txt", "not run"),
    });
    Plan {
        top: format!("ternvale-report-{name}-{stamp}"),
        header,
        items,
    }
}

/// Doctor's FAIL lines, so the README opens with the likely cause.
fn doctor_failures(inputs: &PlanInputs<'_>) -> Vec<String> {
    inputs.doctor.as_deref().map_or_else(Vec::new, |text| {
        text.lines()
            .filter(|line| line.starts_with("FAIL"))
            .map(|line| format!("doctor: {line}"))
            .collect()
    })
}

/// `serial_log` from a config, even one that fails validation.
fn serial_log_of(config: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(config).ok()?;
    ternvale_config::VmConfig::parse(&text)
        .ok()
        .map(|c| c.serial_log)
}

/// The config recorded by the newest run of `name`, for doctor's config checks.
fn manifest_config(dir: &Path, name: &str) -> Option<PathBuf> {
    let log = crate::logs::newest(dir, name).ok()??;
    diag::read_manifest(&diag::sidecar(&log, RUN_SUFFIX))
        .ok()
        .map(|m| m.config)
}

fn sidecar_missing(entry: &str, path: &Path) -> Item {
    Item::missing(
        entry,
        format!(
            "{} does not exist (the run predates this feature, crashed before stopping, or never built a guest)",
            path.display()
        ),
    )
}

/// Decompile a DTB with `dtc`; the error says how to get `dtc`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(dtb = %dtb.display()))]
pub fn dts(dtb: &Path) -> Result<String, String> {
    let output = Command::new("dtc")
        .args(["-q", "-I", "dtb", "-O", "dts"])
        .arg(dtb)
        .output()
        .map_err(|error| format!("dtc not runnable ({error}); `brew install dtc` to get a DTS"))?;
    if !output.status.success() {
        return Err(format!(
            "dtc failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Ask a running `name` to write its diagnostics now.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name))]
pub fn dump_live(name: &str) -> Result<Live> {
    let socket = crate::paths::socket_path(name)?;
    let Ok(stream) = crate::client::connect(&socket)? else {
        return Ok(Live::NotRunning);
    };
    let response = crate::client::exchange(stream, &Request::DumpDiagnostics)
        .with_context(|| format!("ask vm {name} to dump diagnostics"))?;
    Ok(match (response.ok, response.files, response.error) {
        (true, Some(files), _) => Live::Dumped(files),
        (_, _, error) => Live::Failed(error.unwrap_or_else(|| "no files in the response".into())),
    })
}

/// `ternvale report`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name))]
pub fn report(name: &str, out: Option<&Path>, config: Option<&Path>) -> Result<ExitCode> {
    let dir = LogConfig::default_log_dir().context("find the log directory")?;
    let live = dump_live(name).unwrap_or_else(|error| Live::Failed(format!("{error:#}")));
    tracing::info!(target: "ternvale::cli", live = ?live, "report diagnostics source");
    let vm_config = config
        .map(Path::to_path_buf)
        .or_else(|| manifest_config(&dir, name));
    let doctor = crate::doctor::render(&crate::doctor::all_checks(vm_config.as_deref()));
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let plan = plan(
        &dir,
        &PlanInputs {
            name,
            config,
            live,
            doctor: Some(doctor),
        },
        &stamp,
    )?;
    let out = out.map_or_else(
        || PathBuf::from(format!("{}.zip", plan.top)),
        Path::to_path_buf,
    );
    let readme = bundle::write_zip(&out, &plan.top, &plan.header, &plan.items)?;
    print!("{readme}");
    println!("\nwrote {}", out.display());
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
