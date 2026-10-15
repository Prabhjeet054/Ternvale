//! Windows installer scenario: `profile = "windows"` with the Windows 11 ARM64
//! ISO (`./scripts/fetch-windows-iso.sh`, or `TERNVALE_WINDOWS_ISO`) and
//! virtio-win in the `[iso]` slots and no disk, on the pinned release EDK2.
//!
//! There is no display yet (Phase I), so each stage is proved from the serial
//! console or the host:
//! 1. **recognized**: the fw_cfg `bootorder` names only the installer, EDK2
//!    enumerates both discs, and BdsDxe's first attempt loads the installer
//!    from `PciRoot(0x0)/Pci(0x1,0x0)`;
//! 2. **prompt**: the disc's `\EFI\BOOT\BOOTAA64.EFI` prints "Press any key to
//!    boot from CD or DVD" on the serial console; the harness presses Enter;
//! 3. **WinPE loading**: Windows Boot Manager reads `sources\boot.wim` into a
//!    RAM disk; the installer disc's read bytes must pass the size of
//!    `boot.wim` (what the "loading files" screen shows on a display).
//!
//! Later stages are observed and reported, not required:
//! 4. **handoff**: winload's `ExitBootServices` runs VirtioBlkDxe's exit hook,
//!    which resets the installer disc from DRIVER_OK (`virtio status reset
//!    from=15`). The drivers disc is never connected by BDS, so it stays put;
//! 5. **kernel**: the WinPE kernel starts CPU 1 with PSCI `CPU_ON`, which
//!    EDK2 never calls (`cpu_on accepted`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_config::{BlockDevice, BlockRole, VmConfig};
use ternvale_devices::{attach_blocks, last_lines, BlkStats, Pl011, Step};
use ternvale_vmm::{Machine, MachineError};

use crate::common::{
    assets_root, drive, init_logging, log_dir, restore_stdin, stdin_pipe, write_result,
};

const PROMPT: &str = "Press any key to boot from CD or DVD";
/// The installer's transport name in host logs (`[iso] installer` is slot 1).
const INSTALLER_TRANSPORT: &str = "name=virtio-pci-00:01.0";

pub fn run() -> Result<(), String> {
    let iso = windows_iso()?;
    let drivers = assets_root().join("virtio-win/virtio-win.iso");
    if !drivers.is_file() {
        return Err(format!(
            "missing {} (run ./scripts/fetch-virtio-win.sh)",
            drivers.display()
        ));
    }
    let boot_wim = boot_wim_bytes(&iso)?;
    let log_dir = log_dir();
    let guard = init_logging("windows-installer", &log_dir)?;
    let host = guard.log_path().to_path_buf();
    let text = format!(
        "name = \"win-installer\"\nprofile = \"windows\"\nfirmware = \"{}\"\nnvram = \"{}\"\nserial_log = \"{}\"\n\n\
         [iso]\ninstaller = \"{}\"\ndrivers = \"{}\"\n",
        crate::common::firmware().display(),
        log_dir.join("nvram.fd").display(),
        log_dir.join("guest-serial.log").display(),
        iso.display(),
        drivers.display(),
    );
    std::fs::write(log_dir.join("win-installer.toml"), &text)
        .map_err(|err| format!("write toml: {err}"))?;
    let vm = VmConfig::from_toml(&text).map_err(|err| format!("config: {err}"))?;
    tracing::info!(
        target: "ternvale::boot",
        iso = %iso.display(),
        boot_wim_bytes = boot_wim,
        cpus = vm.cpus,
        ram_mib = vm.ram_mib,
        "windows installer boot"
    );
    let started = Instant::now();
    let reached = boot(&vm, &log_dir, &host, boot_wim)?;
    check(&vm, &host, &reached)?;
    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        prompt_ms = reached.prompt_ms,
        winpe_ms = reached.winpe_ms,
        handoff_ms = reached.handoff_ms,
        kernel_ms = reached.kernel_ms,
        scenario = "windows-installer",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

/// `TERNVALE_WINDOWS_ISO`, else `test-assets/windows/windows.iso`.
fn windows_iso() -> Result<PathBuf, String> {
    let path = std::env::var_os("TERNVALE_WINDOWS_ISO")
        .map(PathBuf::from)
        .unwrap_or_else(|| assets_root().join("windows/windows.iso"));
    if path.is_file() {
        return Ok(path);
    }
    Err(format!(
        "missing {} (run ./scripts/fetch-windows-iso.sh '<link>' <sha256>, see the script)",
        path.display()
    ))
}

/// Size of `sources/boot.wim` on the disc's UDF side (the ISO 9660 side holds
/// only `README.TXT`), from `7zz l -slt` (`brew install sevenzip`).
fn boot_wim_bytes(iso: &Path) -> Result<u64, String> {
    let out = std::process::Command::new("7zz")
        .args(["l", "-slt"])
        .arg(iso)
        .arg("sources/boot.wim")
        .output()
        .map_err(|err| format!("7zz (brew install sevenzip): {err}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .skip_while(|line| {
            !line
                .replace('\\', "/")
                .eq_ignore_ascii_case("Path = sources/boot.wim")
        })
        .find_map(|line| line.strip_prefix("Size = "))
        .and_then(|size| size.trim().parse().ok())
        .filter(|size| out.status.success() && *size > 0)
        .ok_or_else(|| {
            format!(
                "{}: no sources/boot.wim size in {text:?} ({})",
                iso.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )
        })
}

/// Milliseconds from VM start to each stage, `None` when not reached.
#[derive(Debug, Default)]
struct Reached {
    prompt_ms: Option<u64>,
    winpe_ms: Option<u64>,
    handoff_ms: Option<u64>,
    kernel_ms: Option<u64>,
    /// Read from the installer disc since the key press.
    installer_bytes: u64,
}

fn boot(vm: &VmConfig, log_dir: &Path, host_log: &Path, boot_wim: u64) -> Result<Reached, String> {
    let uart = Pl011::open(&vm.serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let stats: Arc<Mutex<Option<Arc<BlkStats>>>> = Arc::new(Mutex::new(None));
    let (flag, feed_log, host, watched) = (
        Arc::clone(&cancel),
        vm.serial_log.clone(),
        host_log.to_path_buf(),
        Arc::clone(&stats),
    );
    let started = Instant::now();
    let feeder = std::thread::spawn(move || {
        let mut reached = Reached::default();
        let steps = vec![
            expect("BdsDxe: loading", 180),
            expect(PROMPT, 120),
            Step::Send {
                data: b"\r".to_vec(),
            },
        ];
        let script = drive(&feed_log, &mut input, &flag, &host, steps);
        if script.is_ok() {
            reached.prompt_ms = Some(ms(started));
            watch(&flag, &host, &watched, boot_wim, started, &mut reached);
        }
        flag.store(true, Ordering::Release);
        script.map(|()| reached)
    });
    let devices: Vec<BlockDevice> = vm.block_devices();
    let transport = vm.virtio_transport();
    let exit = Machine::run_with(vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let blocks = attach_blocks(attach, &devices, transport)?;
        let installer = blocks
            .devices
            .iter()
            .find(|block| block.role == BlockRole::Installer)
            .map(|block| Arc::clone(&block.stats));
        *stats
            .lock()
            .map_err(|_| MachineError::Attach("stats lock".into()))? = installer;
        Ok(blocks.mmio)
    });
    cancel.store(true, Ordering::Release);
    let script = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let serial = std::fs::read_to_string(&vm.serial_log).unwrap_or_default();
    let status = script.as_ref().map(|_| ()).map_err(Clone::clone);
    write_result(log_dir, &exit, &status, &serial);
    let fail = |what: String| {
        format!(
            "{what}\nlog={}\n--- serial ---\n{}",
            host_log.display(),
            last_lines(&serial, 30)
        )
    };
    let reached = script.map_err(fail)?;
    match exit {
        Ok(reason) => {
            tracing::info!(target: "ternvale::boot", exit = ?reason, "windows installer vm stopped")
        }
        Err(err) => return Err(fail(format!("exit error {err}"))),
    }
    Ok(reached)
}

fn ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

/// After the key press: wait for the installer to have read `boot.wim`, then
/// for the handoff and the kernel, logging progress every 5 seconds.
fn watch(
    cancel: &AtomicBool,
    host_log: &Path,
    stats: &Mutex<Option<Arc<BlkStats>>>,
    boot_wim: u64,
    started: Instant,
    reached: &mut Reached,
) {
    let winpe_deadline = Instant::now() + Duration::from_secs(secs("TERNVALE_WINPE_SECS", 600));
    let mut stage_deadline = winpe_deadline;
    let mut resets_at_winpe = 0;
    let mut last_report = Instant::now();
    let bytes = || {
        stats
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|stats| stats.snapshot().1))
            .unwrap_or(0)
    };
    let at_prompt = bytes();
    while !cancel.load(Ordering::Acquire) && Instant::now() < stage_deadline {
        std::thread::sleep(Duration::from_millis(250));
        reached.installer_bytes = bytes().saturating_sub(at_prompt);
        let host = std::fs::read_to_string(host_log).unwrap_or_default();
        let resets = host
            .lines()
            .filter(|l| {
                l.contains("virtio status reset")
                    && l.contains(INSTALLER_TRANSPORT)
                    && l.contains("from=15")
            })
            .count();
        if reached.winpe_ms.is_none() && reached.installer_bytes >= boot_wim {
            reached.winpe_ms = Some(ms(started));
            resets_at_winpe = resets;
            stage_deadline =
                Instant::now() + Duration::from_secs(secs("TERNVALE_WINPE_KERNEL_SECS", 180));
            tracing::info!(target: "ternvale::boot", installer_bytes = reached.installer_bytes, boot_wim_bytes = boot_wim, "winpe: boot.wim read from the installer disc");
        }
        if reached.winpe_ms.is_some() && reached.handoff_ms.is_none() && resets > resets_at_winpe {
            reached.handoff_ms = Some(ms(started));
            tracing::info!(target: "ternvale::boot", resets, "winpe: installer disc reset from DRIVER_OK (ExitBootServices)");
        }
        if reached.kernel_ms.is_none() && host.contains("cpu_on accepted") {
            reached.kernel_ms = Some(ms(started));
            tracing::info!(target: "ternvale::boot", "winpe: the kernel started a secondary cpu");
        }
        if reached.kernel_ms.is_some() {
            break;
        }
        if last_report.elapsed() >= Duration::from_secs(5) {
            last_report = Instant::now();
            tracing::info!(target: "ternvale::boot", installer_bytes = reached.installer_bytes, boot_wim_bytes = boot_wim, resets, "winpe progress");
        }
    }
    tracing::info!(target: "ternvale::boot", reached = ?reached, "windows installer stages");
}

fn secs(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(default)
}

fn check(vm: &VmConfig, host_log: &Path, reached: &Reached) -> Result<(), String> {
    let serial =
        crate::uefi_dmem::strip(&std::fs::read_to_string(&vm.serial_log).unwrap_or_default());
    let host = std::fs::read_to_string(host_log).map_err(|err| format!("read host log: {err}"))?;
    let mut problems = Vec::new();
    let order = host
        .lines()
        .find(|line| line.contains("uefi boot order: attached ISO first"));
    if !order
        .is_some_and(|l| l.contains("order=/pci@i0cf8/scsi@1/disk@0,0") && !l.contains("scsi@2"))
    {
        problems.push(format!(
            "boot order line {order:?}, want only the installer (scsi@1)"
        ));
    }
    let identified = host
        .lines()
        .filter(|l| l.contains("guest-visible block device") && l.contains("media=\"optical\""))
        .count();
    if identified != 2 {
        problems.push(format!(
            "{identified} optical discs identified at INFO, want 2"
        ));
    }
    let first = serial
        .lines()
        .find(|l| l.contains("BdsDxe: loading") || l.contains("BdsDxe: failed to load"));
    tracing::info!(target: "ternvale::boot", first_attempt = ?first.map(str::trim_end), "uefi first boot attempt");
    if !first.is_some_and(|l| {
        l.contains("BdsDxe: loading") && l.contains("from PciRoot(0x0)/Pci(0x1,0x0)")
    }) {
        problems.push(format!(
            "first boot attempt {first:?}, want loading the installer at Pci(0x1,0x0)"
        ));
    }
    if reached.prompt_ms.is_none() || !serial.contains(PROMPT) {
        problems.push(format!("no {PROMPT:?} on the serial console"));
    }
    if reached.winpe_ms.is_none() {
        problems.push(format!(
            "the installer disc served {} bytes after the key press, less than boot.wim",
            reached.installer_bytes
        ));
    }
    if problems.is_empty() {
        tracing::info!(target: "ternvale::boot", "windows installer checks passed");
        return Ok(());
    }
    tracing::error!(target: "ternvale::boot", problems = %problems.join("; "), "windows installer checks failed");
    Err(problems.join("; "))
}

fn expect(pattern: &str, secs: u64) -> Step {
    Step::Expect {
        pattern: pattern.to_string(),
        timeout: Duration::from_secs(secs),
    }
}
