//! `validate`, `create-disk`, `status`, `pause`, `resume`, and `stop`.
//!
//! Human-readable results go to stdout; `--json` prints the socket's raw
//! response line instead.

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use ternvale_config::VmConfig;

use crate::client::{self, Reach};
use crate::disk;
use crate::paths;
use crate::protocol::{Request, Response, StatsJson, StatusJson};

/// How long `stop` waits for an orderly exit.
const STOP_WAIT: Duration = Duration::from_secs(30);
const STOP_POLL: Duration = Duration::from_millis(100);

/// `ternvale validate <config>`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(config = %path.display()))]
pub fn validate(path: &Path) -> Result<ExitCode> {
    let config = VmConfig::from_path(path)
        .with_context(|| format!("validate vm config {}", path.display()))?;
    let socket = paths::socket_path(&config.name)
        .with_context(|| format!("vm name {:?} cannot name a control socket", config.name))?;
    print!("{}", describe(path, &config, &socket));
    tracing::info!(target: "ternvale::cli", vm = %config.name, "config valid");
    Ok(ExitCode::SUCCESS)
}

/// Summary printed by `validate`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = %config.name))]
pub fn describe(path: &Path, config: &VmConfig, socket: &Path) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    line(format!("{}: ok", path.display()));
    line(format!("  name        {}", config.name));
    line(format!("  cpus        {}", config.cpus));
    line(format!("  ram         {} MiB", config.ram_mib));
    match &config.firmware {
        Some(firmware) => line(format!("  firmware    {}", firmware.display())),
        None => line(format!("  kernel      {}", config.kernel.display())),
    }
    if let Some(initrd) = &config.initrd {
        line(format!("  initrd      {}", initrd.display()));
    }
    if !config.cmdline.is_empty() {
        line(format!("  cmdline     {}", config.cmdline));
    }
    for (index, item) in config.disks.iter().enumerate() {
        let mode = if item.read_only { "ro" } else { "rw" };
        let boot = if index == 0 && config.boot_disk {
            ", root"
        } else {
            ""
        };
        line(format!(
            "  disk {index}      {} ({mode}{boot})",
            item.path.display()
        ));
    }
    for (index, nic) in config.nics.iter().enumerate() {
        line(format!("  nic {index}       {}", nic.backend));
    }
    line(format!("  serial log  {}", config.serial_log.display()));
    line(format!("  socket      {}", socket.display()));
    out
}

/// `ternvale create-disk <path> <size>`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display(), size))]
pub fn create_disk(path: &Path, size: &str) -> Result<ExitCode> {
    let bytes = disk::parse_size(size).with_context(|| format!("parse disk size {size:?}"))?;
    disk::create_disk(path, bytes)?;
    println!(
        "created {} ({}, sparse)",
        path.display(),
        disk::human_size(bytes)
    );
    Ok(ExitCode::SUCCESS)
}

/// `ternvale status <name>`. A VM that is not running exits 1 without an error.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name))]
pub fn status(name: &str, stats: bool, json: bool) -> Result<ExitCode> {
    let socket = paths::socket_path(name)?;
    let stream = match client::connect(&socket)? {
        Ok(stream) => stream,
        Err(reach) => {
            println!("vm {name}: not running{}", stale_note(&reach, &socket));
            return Ok(ExitCode::FAILURE);
        }
    };
    let request = if stats {
        Request::QueryStats
    } else {
        Request::Status
    };
    let response = client::exchange(stream, &request)?;
    report(name, &request, &response, json)
}

/// `ternvale pause <name>`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name, timeout_ms))]
pub fn pause(name: &str, timeout_ms: u64, json: bool) -> Result<ExitCode> {
    let request = Request::Pause {
        timeout_ms: Some(timeout_ms),
    };
    let response = client::request(&paths::socket_path(name)?, name, &request)?;
    report(name, &request, &response, json)
}

/// `ternvale resume <name>`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name))]
pub fn resume(name: &str, json: bool) -> Result<ExitCode> {
    let request = Request::Resume;
    let response = client::request(&paths::socket_path(name)?, name, &request)?;
    report(name, &request, &response, json)
}

/// `ternvale stop <name>`: send `shutdown` (or `force-stop`), then wait for
/// the VM process to remove its socket unless `no_wait`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name, force, no_wait))]
pub fn stop(name: &str, force: bool, no_wait: bool, json: bool) -> Result<ExitCode> {
    let socket = paths::socket_path(name)?;
    let request = if force {
        Request::ForceStop
    } else {
        Request::Shutdown
    };
    let response = client::request(&socket, name, &request)?;
    let code = report(name, &request, &response, json)?;
    if no_wait {
        return Ok(code);
    }
    let started = Instant::now();
    wait_gone(&socket, STOP_WAIT)
        .with_context(|| format!("wait for vm {name} to exit after {}", request.name()))?;
    tracing::info!(target: "ternvale::cli", vm = name, waited_ms = started.elapsed().as_millis() as u64, "vm exited");
    if !json {
        println!("vm {name}: exited");
    }
    Ok(code)
}

/// Poll until nothing serves `socket`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %socket.display()))]
pub fn wait_gone(socket: &Path, limit: Duration) -> Result<()> {
    let deadline = Instant::now() + limit;
    loop {
        match client::connect(socket)? {
            Err(_) => return Ok(()),
            Ok(stream) => drop(stream),
        }
        if Instant::now() >= deadline {
            bail!("still serving {} after {limit:?}", socket.display());
        }
        std::thread::sleep(STOP_POLL);
    }
}

fn stale_note(reach: &Reach, socket: &Path) -> String {
    match reach {
        Reach::NotRunning => String::new(),
        Reach::Stale => format!(" (stale socket {})", socket.display()),
    }
}

/// Print a response. A refusal is an error naming the VM and the command;
/// with `--json` the raw refusal is printed too and the exit status is 1.
fn report(name: &str, request: &Request, response: &Response, json: bool) -> Result<ExitCode> {
    if !response.ok {
        let error = response.error.as_deref().unwrap_or("no error message");
        tracing::warn!(target: "ternvale::cli", vm = name, cmd = request.name(), error, "vm refused the request");
        if json {
            println!(
                "{}",
                serde_json::to_string(response).context("encode response")?
            );
        }
        bail!("vm {name} refused {}: {error}", request.verb());
    }
    if json {
        println!(
            "{}",
            serde_json::to_string(response).context("encode response")?
        );
        return Ok(ExitCode::SUCCESS);
    }
    let status = response
        .status
        .as_ref()
        .with_context(|| format!("vm {name} answered without a status"))?;
    print!("{}", render(status, response.stats.as_ref()));
    Ok(ExitCode::SUCCESS)
}

/// Human-readable status (and per-vCPU counters).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = %status.name))]
pub fn render(status: &StatusJson, stats: Option<&StatsJson>) -> String {
    let mut lines = vec![
        format!("vm {}: {}", status.name, status.state),
        format!(
            "  cpus {}, up {}, {} for {}",
            status.cpus,
            seconds(status.uptime_ms),
            status.state,
            seconds(status.state_ms)
        ),
    ];
    if status.pauses > 0 {
        lines.push(format!(
            "  paused {} total over {} pause(s)",
            seconds(status.paused_ms),
            status.pauses
        ));
    }
    if let Some(cause) = &status.stop_cause {
        lines.push(format!("  stop cause: {cause}"));
    }
    if let Some(failure) = &status.failure {
        lines.push(format!("  failure: {failure}"));
    }
    if let Some(stats) = stats {
        if let Some(ms) = stats.process_cpu_ms {
            lines.push(format!("  process cpu {}", seconds(ms)));
        }
        lines
            .push("  cpu  in-guest  runs        guest     wfi-parks  parked    vtimer".to_string());
        for cpu in &stats.cpus {
            let Some(runs) = cpu.runs else {
                lines.push(format!(
                    "  {:<4} {:<9} (not created)",
                    cpu.cpu, cpu.in_guest
                ));
                continue;
            };
            lines.push(format!(
                "  {:<4} {:<9} {:<11} {:<9} {:<10} {:<9} {}",
                cpu.cpu,
                cpu.in_guest,
                runs,
                seconds(cpu.guest_ms.unwrap_or(0)),
                cpu.wfi_parks.unwrap_or(0),
                seconds(cpu.park_ms.unwrap_or(0)),
                cpu.vtimer_exits.unwrap_or(0)
            ));
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

fn seconds(ms: u64) -> String {
    format!("{}.{}s", ms / 1000, (ms % 1000) / 100)
}
