//! `ternvale doctor`: can this Mac run a Ternvale VM, and if not, how to fix it.
//!
//! [`probe::gather`] reads the host into [`Facts`]; the `check_*` functions
//! turn facts into pass/warn/fail lines without touching the host, so every
//! verdict and fix text is unit-tested. With `--config`, [`vm`] also checks
//! the files and cmdline of one VM config.

pub mod probe;
pub mod vm;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use serde::Serialize;
use ternvale_hv::{MacosVersion, GIC_MIN_MACOS_MAJOR};

/// Free space below this fails the disk check.
pub const DISK_FAIL_BYTES: u64 = 1 << 30;
/// Free space below this warns.
pub const DISK_WARN_BYTES: u64 = 5 << 30;
/// Log directory size above this warns.
pub const LOG_DIR_WARN_BYTES: u64 = 1 << 30;
/// The entitlement every `ternvale` binary needs.
pub const HV_ENTITLEMENT: &str = "com.apple.security.hypervisor";

/// One check's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Pass,
    Warn,
    Fail,
    /// Not run because an earlier check failed.
    Skip,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: String,
    pub outcome: Outcome,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    pub(crate) fn new(
        name: impl Into<String>,
        outcome: Outcome,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            outcome,
            detail: detail.into(),
            fix: None,
        }
    }

    pub(crate) fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// Why creating a probe VM failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmProbe {
    /// `HV_DENIED`: the binary lacks the hypervisor entitlement.
    Denied,
    Other(String),
}

/// Free space on the volume holding `path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFacts {
    pub path: PathBuf,
    pub free_bytes: u64,
    pub total_bytes: u64,
}

/// The log directory after a create, write, and delete probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogDirFacts {
    pub path: PathBuf,
    pub files: u64,
    pub bytes: u64,
}

/// Everything the checks look at.
#[derive(Debug, Clone)]
pub struct Facts {
    pub macos: Result<MacosVersion, String>,
    pub hv_support: Result<bool, String>,
    pub exe: PathBuf,
    /// `codesign -d --entitlements - --xml <exe>` stdout, or why it could not run.
    pub entitlements: Result<String, String>,
    pub vm: Result<(), VmProbe>,
    /// `None` when there was no probe VM to put a GIC in.
    pub gic: Option<Result<(), String>>,
    pub disk: Result<DiskFacts, String>,
    pub log_dir: Result<LogDirFacts, String>,
}

/// macOS 15 or newer (the in-kernel GICv3 needs it).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn check_macos(macos: &Result<MacosVersion, String>) -> Check {
    const NAME: &str = "macOS version";
    match macos {
        Ok(v) if v.major >= GIC_MIN_MACOS_MAJOR => {
            Check::new(NAME, Outcome::Pass, format!("macOS {}", v.text))
        }
        Ok(v) => Check::new(
            NAME,
            Outcome::Fail,
            format!(
                "macOS {} is older than {GIC_MIN_MACOS_MAJOR}.0, which the in-kernel GICv3 needs",
                v.text
            ),
        )
        .fix(format!(
            "Update macOS to {GIC_MIN_MACOS_MAJOR} or newer (System Settings > General > Software Update)."
        )),
        Err(error) => Check::new(NAME, Outcome::Warn, format!("could not read: {error}"))
            .fix("Check `sw_vers -productVersion` by hand."),
    }
}

/// `kern.hv_support`, then whether a VM can actually be created.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn check_hypervisor(facts: &Facts) -> Check {
    const NAME: &str = "hypervisor";
    match (&facts.hv_support, &facts.vm) {
        (Ok(false), _) => Check::new(
            NAME,
            Outcome::Fail,
            "kern.hv_support is 0: this Mac (or the VM it runs in) has no Hypervisor.framework",
        )
        .fix("Run on an Apple silicon Mac directly; most macOS VMs do not offer nested virtualization."),
        (_, Ok(())) => Check::new(NAME, Outcome::Pass, "created and destroyed a probe VM"),
        (_, Err(VmProbe::Denied)) => Check::new(
            NAME,
            Outcome::Fail,
            "hv_vm_create returned HV_DENIED: the binary lacks the hypervisor entitlement",
        )
        .fix(sign_fix(&facts.exe)),
        (_, Err(VmProbe::Other(error))) => {
            Check::new(NAME, Outcome::Fail, format!("hv_vm_create failed: {error}")).fix(
                "Quit other VMMs that may hold the hypervisor, then retry; see docs/DEBUGGING.md.",
            )
        }
    }
}

/// The running binary carries `com.apple.security.hypervisor`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn check_entitlement(facts: &Facts) -> Check {
    const NAME: &str = "entitlement";
    let exe = facts.exe.display();
    match &facts.entitlements {
        Ok(out) if has_hv_entitlement(out) => {
            Check::new(NAME, Outcome::Pass, format!("{HV_ENTITLEMENT} on {exe}"))
        }
        Ok(_) => Check::new(NAME, Outcome::Fail, format!("{exe} lacks {HV_ENTITLEMENT}"))
            .fix(sign_fix(&facts.exe)),
        Err(error) => Check::new(
            NAME,
            Outcome::Warn,
            format!("could not read the signature of {exe}: {error}"),
        )
        .fix(format!("Run `codesign -d --entitlements - {exe}` by hand.")),
    }
}

/// An in-kernel GICv3 can be created in the probe VM.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn check_gic(gic: &Option<Result<(), String>>) -> Check {
    const NAME: &str = "GICv3";
    match gic {
        None => Check::new(NAME, Outcome::Skip, "needs a probe VM (see hypervisor)"),
        Some(Ok(())) => Check::new(NAME, Outcome::Pass, "in-kernel GICv3 created"),
        Some(Err(error)) => Check::new(
            NAME,
            Outcome::Fail,
            format!("hv_gic_create failed: {error}"),
        )
        .fix(format!(
            "The in-kernel GIC needs macOS {GIC_MIN_MACOS_MAJOR}+ on Apple silicon; update macOS."
        )),
    }
}

/// Room for logs, serial output, and crash reports.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn check_disk(disk: &Result<DiskFacts, String>) -> Check {
    const NAME: &str = "disk space";
    match disk {
        Ok(d) => {
            let detail = format!(
                "{} free of {} on the volume holding {}",
                human(d.free_bytes),
                human(d.total_bytes),
                d.path.display()
            );
            let fix =
                "Free space on that volume (empty the Trash, delete old disk images or logs).";
            if d.free_bytes < DISK_FAIL_BYTES {
                Check::new(NAME, Outcome::Fail, detail).fix(fix)
            } else if d.free_bytes < DISK_WARN_BYTES {
                Check::new(NAME, Outcome::Warn, detail).fix(fix)
            } else {
                Check::new(NAME, Outcome::Pass, detail)
            }
        }
        Err(error) => Check::new(NAME, Outcome::Warn, format!("could not measure: {error}"))
            .fix("Check `df -h ~` by hand."),
    }
}

/// The log directory exists, is writable, and is not huge.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn check_log_dir(log_dir: &Result<LogDirFacts, String>) -> Check {
    const NAME: &str = "log directory";
    match log_dir {
        Ok(l) if l.bytes > LOG_DIR_WARN_BYTES => Check::new(
            NAME,
            Outcome::Warn,
            format!(
                "{} is writable but holds {} in {} files",
                l.path.display(),
                human(l.bytes),
                l.files
            ),
        )
        .fix(format!(
            "Delete old logs: `find {} -name 'ternvale-*' -mtime +14 -delete`.",
            l.path.display()
        )),
        Ok(l) => Check::new(
            NAME,
            Outcome::Pass,
            format!(
                "{} is writable ({} files, {})",
                l.path.display(),
                l.files,
                human(l.bytes)
            ),
        ),
        Err(error) => Check::new(NAME, Outcome::Fail, error.clone())
            .fix("Make ~/Library/Logs/Ternvale a writable directory owned by you."),
    }
}

/// Every check, in report order.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn evaluate(facts: &Facts) -> Vec<Check> {
    vec![
        check_macos(&facts.macos),
        check_hypervisor(facts),
        check_entitlement(facts),
        check_gic(&facts.gic),
        check_disk(&facts.disk),
        check_log_dir(&facts.log_dir),
    ]
}

/// Aligned `PASS  name  detail` lines with `fix:` lines and a totals line.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn render(checks: &[Check]) -> String {
    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let mut lines = Vec::new();
    for check in checks {
        lines.push(format!(
            "{}  {:width$}  {}",
            check.outcome.label(),
            check.name,
            check.detail
        ));
        if let Some(fix) = &check.fix {
            lines.push(format!("      {:width$}  fix: {fix}", ""));
        }
    }
    let count = |o: Outcome| checks.iter().filter(|c| c.outcome == o).count();
    lines.push(format!(
        "{} passed, {} warnings, {} failed, {} skipped",
        count(Outcome::Pass),
        count(Outcome::Warn),
        count(Outcome::Fail),
        count(Outcome::Skip)
    ));
    lines.join("\n")
}

/// Host checks, then the checks for `config` when one is given.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(config = ?config))]
pub fn all_checks(config: Option<&std::path::Path>) -> Vec<Check> {
    let mut checks = evaluate(&probe::gather());
    if let Some(config) = config {
        checks.extend(vm::checks_for_path(config));
    }
    checks
}

/// `ternvale doctor [--config vm.toml]`: exit 1 when any check fails.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(json, config = ?config))]
pub fn doctor(json: bool, config: Option<&std::path::Path>) -> Result<ExitCode> {
    let checks = all_checks(config);
    for check in &checks {
        tracing::info!(target: "ternvale::cli", check = %check.name, outcome = ?check.outcome, detail = %check.detail, "doctor check");
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&checks).context("encode doctor report")?
        );
    } else {
        println!("{}", render(&checks));
    }
    Ok(if checks.iter().any(|c| c.outcome == Outcome::Fail) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// `codesign` output (XML plist or the newer `[Key] … [Bool] true` form)
/// grants the hypervisor entitlement.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn has_hv_entitlement(codesign: &str) -> bool {
    let Some(at) = codesign.find(HV_ENTITLEMENT) else {
        return false;
    };
    let rest = &codesign[at + HV_ENTITLEMENT.len()..];
    let value = rest
        .find("<key>")
        .into_iter()
        .chain(rest.find("[Key]"))
        .min()
        .map_or(rest, |end| &rest[..end]);
    value.contains("<true/>") || value.contains("[Bool] true")
}

fn sign_fix(exe: &std::path::Path) -> String {
    format!(
        "Sign it: `codesign --sign - --force --entitlements entitlements/ternvale.entitlements {}` (from the repo root), or run through scripts/sign-and-run.sh.",
        exe.display()
    )
}

/// Bytes in KiB/MiB/GiB/TiB with one decimal.
#[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
pub fn human(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod tests;
