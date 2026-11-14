//! Command-line grammar.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::display::DisplayMode;
use crate::logfmt::Level;

/// Ternvale: an ARM64 VMM on Hypervisor.framework.
#[derive(Debug, Parser)]
#[command(name = "ternvale", version, about)]
pub struct Cli {
    /// Print machine-readable JSON on stdout (every command).
    #[arg(long, global = true)]
    pub json: bool,
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// One `ternvale` subcommand.
#[derive(Debug, Subcommand, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)] // `Create` grew `--macos` / `--ipsw` / `--sha256`.
pub enum Command {
    /// Listen on the JSON-RPC hub socket (`run/api.sock`) until killed.
    Serve,
    /// Boot the VM described by a TOML config and serve its control socket
    /// until it stops.
    Run {
        /// VM config (TOML).
        config: PathBuf,
        /// Host display: `none`, or `window` to add a virtio-gpu and show it
        /// in the dev viewer (build it with `make viewer`).
        #[arg(long, value_enum, default_value_t = DisplayMode::None)]
        display: DisplayMode,
        /// Frame-rate cap for `--display window`.
        #[arg(long, default_value_t = ternvale_gpu::DEFAULT_FPS,
            value_parser = clap::value_parser!(u32).range(1..=i64::from(ternvale_gpu::MAX_FPS)))]
        fps: u32,
        /// No virtio-gpu / viewer (same as `--display none`).
        #[arg(long)]
        headless: bool,
    },
    /// Run a command in the guest via the agent (`agent.allow_exec`).
    Exec {
        /// VM name.
        name: String,
        /// Kill the guest process after this many seconds.
        #[arg(long)]
        timeout: Option<u64>,
        /// Program and arguments (put them after `--`).
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Copy a file between host and guest (`host /path` and `vm:/path`).
    Cp {
        /// Source (`path` or `vm:/path`).
        src: String,
        /// Destination (`path` or `vm:/path`).
        dst: String,
    },
    /// Copy `guest-tools/ternvale-guest-setup` into a Linux guest and run it
    /// (Rosetta mount, binfmt, multiarch, Docker/Podman). Needs `agent.allow_exec`.
    GuestSetup {
        /// VM name.
        name: String,
        /// Log guest actions without writing files.
        #[arg(long)]
        dry_run: bool,
        /// Run the guest undo script instead of applying setup.
        #[arg(long)]
        undo: bool,
    },
    /// Wait until the guest agent is connected.
    WaitReady {
        /// VM name.
        name: String,
        /// Give up after this many seconds.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Open an SSH session (port forward + identity in the VM library dir).
    Ssh {
        /// VM name.
        name: String,
    },
    /// Parse and check a VM config without starting anything.
    Validate {
        /// VM config (TOML).
        config: PathBuf,
    },
    /// Create a library VM (`--windows --iso --unattended` or `--macos`).
    Create {
        /// Guest name under `~/Library/Application Support/Ternvale/vms/`.
        #[arg(long)]
        name: String,
        /// Windows 11 ARM64 profile.
        #[arg(long)]
        windows: bool,
        /// macOS guest on the VZ backend (restore image + installer).
        #[arg(long)]
        macos: bool,
        /// User-supplied macOS restore image (`.ipsw`). Omitted: fetch latest supported.
        #[arg(long)]
        ipsw: Option<PathBuf>,
        /// Expected SHA-256 of the restore image (hex). Verified after download.
        #[arg(long)]
        sha256: Option<String>,
        /// User-supplied Windows installer ISO (never bundled).
        #[arg(long)]
        iso: Option<PathBuf>,
        /// Generate Autounattend.xml and a small answer-file ISO.
        #[arg(long)]
        unattended: bool,
        /// Windows image name (`/IMAGE/NAME`).
        #[arg(long, default_value = crate::create_cmd::DEFAULT_EDITION)]
        edition: String,
        /// System disk size (default 64G).
        #[arg(long, default_value = crate::create_cmd::DEFAULT_DISK)]
        disk_size: String,
        /// vCPUs for `--macos` (raised to the restore image minimum).
        #[arg(long, default_value_t = crate::create_macos::DEFAULT_CPUS)]
        cpus: u32,
        /// Guest RAM in MiB for `--macos` (raised to the restore image minimum).
        #[arg(long, default_value_t = crate::create_macos::DEFAULT_RAM_MIB)]
        ram_mib: u64,
        /// UEFI firmware (`QEMU_EFI.fd`).
        #[arg(long)]
        firmware: Option<PathBuf>,
        /// `ternvale-tools.iso` (virtio-win + agent).
        #[arg(long)]
        tools_iso: Option<PathBuf>,
        /// Local administrator account.
        #[arg(long, default_value = crate::create_cmd::DEFAULT_USER)]
        user: String,
        /// Env var that holds the account password (never logged).
        #[arg(long)]
        password_env: Option<String>,
        /// File that holds the account password (never logged).
        #[arg(long)]
        password_file: Option<PathBuf>,
        /// Env var that holds the Windows product key (optional; never generated).
        #[arg(long)]
        product_key_env: Option<String>,
        /// File that holds the Windows product key (optional).
        #[arg(long)]
        product_key_file: Option<PathBuf>,
        /// Spawn `ternvale run` and wait until the guest agent connects.
        #[arg(long)]
        wait: bool,
        /// `--wait` timeout in seconds.
        #[arg(long, default_value_t = 7200)]
        timeout_secs: u64,
    },
    /// Compact layered disks or grow a disk online.
    Disk {
        #[command(subcommand)]
        action: DiskCommand,
    },
    /// Create a sparse raw disk image.
    CreateDisk {
        /// Image path; must not exist yet.
        path: PathBuf,
        /// Size: bytes, or a number with K, M, G, or T (powers of 1024).
        /// Must be a multiple of 512.
        size: String,
    },
    /// Set virtio-balloon size (`balloon-set`) or resume auto policy.
    Balloon {
        /// VM name (the config's `name`).
        name: String,
        /// Inflate to this many MiB of ballooned pages (capped by floor / in-use).
        #[arg(long)]
        target_mib: Option<u64>,
        /// Follow host memory pressure again.
        #[arg(long)]
        auto: bool,
    },
    /// Show lock-free VM metrics (`query-stats`).
    Stats {
        /// VM name (the config's `name`).
        name: String,
        /// Repeat until the VM exits.
        #[arg(long)]
        watch: bool,
        /// `--watch` poll period in milliseconds (default 1000).
        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,
    },
    /// Show a running VM's state.
    Status {
        /// VM name (the config's `name`).
        name: String,
        /// Also show per-vCPU counters (`query-stats`).
        #[arg(long)]
        stats: bool,
    },
    /// Stop every vCPU of a running VM; guest time stands still until resume.
    Pause {
        /// VM name.
        name: String,
        /// Give up if the vCPUs have not all stopped after this many milliseconds.
        #[arg(long, default_value_t = 5000)]
        timeout_ms: u64,
    },
    /// Full-VM snapshots (guest RAM, GIC, vCPU registers).
    Snapshot {
        #[command(subcommand)]
        action: SnapshotCommand,
    },
    /// Linked clone: new VM config sharing the source disk base read-only.
    Clone {
        /// Source VM name (under `~/Library/Application Support/Ternvale/vms/`).
        source: String,
        /// New VM name.
        new_name: String,
    },
    /// Write a portable `.tvm` directory bundle (manifest, disks, NVRAM, TPM).
    Export {
        /// Library VM name.
        vm: String,
        /// Output directory (convention: `name.tvm`).
        out: PathBuf,
        /// zstd-compress disk and boot payloads.
        #[arg(long)]
        zstd: bool,
    },
    /// Import a `.tvm` bundle or a qcow2/vmdk/vhdx/raw disk (`qemu-img`).
    Import {
        /// `.tvm` directory or a disk image.
        source: PathBuf,
        /// Library VM name (required for disk images; default: bundle `source_vm`).
        #[arg(long)]
        name: Option<String>,
        /// After `qemu-img convert`, wrap the raw image in a linked-clone stack.
        #[arg(long)]
        layered: bool,
        /// Kernel for a foreign disk import (direct boot).
        #[arg(long)]
        kernel: Option<PathBuf>,
        /// UEFI firmware for a foreign disk import.
        #[arg(long)]
        firmware: Option<PathBuf>,
    },
    /// Freeze a VM as a read-only template.
    Template {
        #[command(subcommand)]
        action: TemplateCommand,
    },
    /// Create a VM from a template (linked clone).
    New {
        /// New VM name.
        name: String,
        /// Template name (`ternvale template create`).
        #[arg(long)]
        from_template: String,
    },
    /// VM library maintenance.
    Vm {
        #[command(subcommand)]
        action: VmCommand,
    },
    /// Continue a paused VM.
    Resume {
        /// VM name.
        name: String,
    },
    /// Stop a VM: orderly host-side shutdown, or `--force` to stop at once.
    Stop {
        /// VM name.
        name: String,
        /// Send `force-stop`: the VM process exits even if teardown hangs.
        #[arg(long)]
        force: bool,
        /// Pulse the guest power button and wait for a clean OS shutdown.
        #[arg(long, conflicts_with = "force")]
        graceful: bool,
        /// Return once the request is accepted instead of waiting for the VM to exit.
        #[arg(long)]
        no_wait: bool,
    },
    /// Check this Mac can run VMs: macOS version, hypervisor, entitlement,
    /// GIC, disk space, and the log directory. Exits 1 if a check fails.
    Doctor {
        /// Also check this VM config: kernel, initrd, disks, and cmdline.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Download Rosetta from Apple after typing `yes`. Never silent.
        #[arg(long)]
        install_rosetta: bool,
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
    /// Compare this binary's version to a JSON update feed (file only; no install).
    UpdateCheck {
        /// Feed JSON (`docs/updates/latest.json` or a downloaded copy of the hosted URL).
        #[arg(long)]
        feed: PathBuf,
    },
    /// Guest tools ISO (`ternvale-tools.iso`).
    Tools {
        #[command(subcommand)]
        action: crate::tools::ToolsCommand,
    },
    /// At-rest encryption: rotate the wrapping key without re-encrypting data.
    Crypto {
        #[command(subcommand)]
        action: CryptoCommand,
    },
    /// Check a signed `.app`: `codesign --verify --deep --strict`, hypervisor entitlements, `spctl`.
    ReleaseVerify {
        /// Path to `Ternvale.app`.
        #[arg(long)]
        app: PathBuf,
        /// Fail if Gatekeeper (`spctl -a -vvv`) rejects the bundle (set after notarize + staple).
        #[arg(long)]
        expect_gatekeeper: bool,
    },
}

/// `ternvale template …` subcommands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum TemplateCommand {
    /// Copy `vm` into the template library with read-only disks.
    Create {
        /// Source VM name.
        vm: String,
        /// Template name.
        name: String,
    },
}

/// `ternvale vm …` subcommands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum VmCommand {
    /// Remove a VM from the library and release shared disk base refcounts.
    Delete {
        /// VM name.
        name: String,
    },
}

/// `ternvale crypto …` subcommands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum CryptoCommand {
    /// Re-wrap the data key with a new Keychain wrapping key (no data rewrite).
    Rotate {
        /// VM name.
        name: String,
    },
}

/// `ternvale disk …` subcommands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum DiskCommand {
    /// Rewrite delta layers, dropping unmapped blocks (VM must be stopped).
    Compact {
        /// VM name.
        vm: String,
        /// Disk index (`[[disks]]` order, default 0).
        #[arg(long)]
        disk: Option<u32>,
    },
    /// Grow (or forced-shrink) a disk; running VMs get a virtio config change.
    Resize {
        /// VM name.
        vm: String,
        /// New size (`4G`, `8192M`, or bytes).
        size: String,
        /// Allow shrinking below the in-use size.
        #[arg(long)]
        force: bool,
        /// Disk index (`[[disks]]` order, default 0).
        #[arg(long)]
        disk: Option<u32>,
    },
}

/// `ternvale snapshot …` subcommands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum SnapshotCommand {
    /// Capture guest RAM, GIC, and vCPU state into a named bundle.
    Create {
        /// VM name.
        vm: String,
        /// Snapshot label.
        name: String,
    },
    /// Restore a bundle into a running (paused) VM.
    Restore {
        /// VM name.
        vm: String,
        /// Snapshot label.
        name: String,
    },
    /// List snapshot labels for a VM.
    List {
        /// VM name.
        vm: String,
    },
    /// Delete a snapshot bundle.
    Delete {
        /// VM name.
        vm: String,
        /// Snapshot label.
        name: String,
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
