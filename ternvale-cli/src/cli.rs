//! Command-line grammar.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::logfmt::Level;

/// Ternvale: an ARM64 VMM on Hypervisor.framework.
#[derive(Debug, Parser)]
#[command(name = "ternvale", version, about)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// One `ternvale` subcommand.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// Boot the VM described by a TOML config and serve its control socket
    /// until it stops.
    Run {
        /// VM config (TOML).
        config: PathBuf,
    },
    /// Parse and check a VM config without starting anything.
    Validate {
        /// VM config (TOML).
        config: PathBuf,
    },
    /// Create a sparse raw disk image.
    CreateDisk {
        /// Image path; must not exist yet.
        path: PathBuf,
        /// Size: bytes, or a number with K, M, G, or T (powers of 1024).
        /// Must be a multiple of 512.
        size: String,
    },
    /// Show a running VM's state.
    Status {
        /// VM name (the config's `name`).
        name: String,
        /// Also show per-vCPU counters (`query-stats`).
        #[arg(long)]
        stats: bool,
        /// Print the raw JSON response.
        #[arg(long)]
        json: bool,
    },
    /// Stop every vCPU of a running VM; guest time stands still until resume.
    Pause {
        /// VM name.
        name: String,
        /// Give up if the vCPUs have not all stopped after this many milliseconds.
        #[arg(long, default_value_t = 5000)]
        timeout_ms: u64,
        /// Print the raw JSON response.
        #[arg(long)]
        json: bool,
    },
    /// Continue a paused VM.
    Resume {
        /// VM name.
        name: String,
        /// Print the raw JSON response.
        #[arg(long)]
        json: bool,
    },
    /// Stop a VM: orderly host-side shutdown, or `--force` to stop at once.
    Stop {
        /// VM name.
        name: String,
        /// Send `force-stop`: the VM process exits even if teardown hangs.
        #[arg(long)]
        force: bool,
        /// Return once the request is accepted instead of waiting for the VM to exit.
        #[arg(long)]
        no_wait: bool,
        /// Print the raw JSON response.
        #[arg(long)]
        json: bool,
    },
    /// Check this Mac can run VMs: macOS version, hypervisor, entitlement,
    /// GIC, disk space, and the log directory. Exits 1 if a check fails.
    Doctor {
        /// Print the checks as JSON.
        #[arg(long)]
        json: bool,
        /// Also check this VM config: kernel, initrd, disks, and cmdline.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Show a VM's newest host log, filtered by level and target.
    Logs {
        /// VM name (the config's `name`; `cli` for non-run commands).
        name: String,
        /// Keep printing new lines; switches to a newer log if the VM restarts.
        #[arg(short, long)]
        follow: bool,
        /// Minimum level: trace, debug, info, warn, or error.
        #[arg(long)]
        level: Option<Level>,
        /// Only these targets and their children, e.g. `ternvale::virtio`
        /// or `virtio::blk`. Repeat for several.
        #[arg(long = "target")]
        targets: Vec<String>,
        /// List the VM's log files instead of printing one.
        #[arg(long)]
        list: bool,
        /// Read this log file instead of the newest one.
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Zip host log, config, serial log, DTB, last MMIO events, and summary
    /// for a bug report. Asks a running VM to dump its diagnostics first.
    #[command(alias = "bundle")]
    Report {
        /// VM name.
        name: String,
        /// Output zip (default ./ternvale-report-<name>-<stamp>.zip).
        #[arg(long)]
        out: Option<PathBuf>,
        /// VM config to include when the run left no manifest.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Build a probe VM's ACPI window, read the tables back from guest memory
    /// into <out>/ternvale, and optionally diff them against a QEMU RAM image.
    /// With --offline the window is a host buffer: same tables, no VM.
    AcpiDump {
        /// Output directory.
        #[arg(long)]
        out: PathBuf,
        /// RAM saved from QEMU `-M virt` with `pmemsave` (see scripts/acpi-compare.sh).
        #[arg(long)]
        qemu_ram: Option<PathBuf>,
        /// Guest physical address of the image's first byte.
        #[arg(long, default_value = "0x40000000", value_parser = parse_u64)]
        ram_base: u64,
        /// vCPUs the probe VM's tables describe (one MADT GICC each).
        #[arg(long, default_value_t = 1)]
        cpus: u32,
        /// Build and walk the tables in host memory instead of a VM; needs no
        /// hypervisor entitlement (scripts/acpi-check.sh).
        #[arg(long)]
        offline: bool,
    },
}

/// A decimal or `0x` hexadecimal u64.
fn parse_u64(text: &str) -> Result<u64, String> {
    let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.map_err(|error| format!("{text:?} is not a u64: {error}"))
}
