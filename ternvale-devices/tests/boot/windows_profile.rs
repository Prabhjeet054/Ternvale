//! Windows profile scenario: the fw_cfg `bootorder` makes EDK2 boot the
//! attached ISO first even though it is the second disk.
//!
//! Two boots with the same two virtio-pci disks: a blank 64 GiB sparse
//! system disk at 00:01.0 and the Alpine ISO
//! (`./scripts/fetch-installer-iso.sh`) at 00:02.0. Both configs are TOML
//! documents parsed and validated like `ternvale run` does.
//! - `control`: `profile` unset (linux), no `bootorder`. EDK2's first boot
//!   attempt must be `PciRoot(0x0)/Pci(0x1,0x0)`, the blank disk (`failed to
//!   load ... Not Found`), which shows the second boot's order is not EDK2's
//!   default.
//! - `windows`: `profile = "windows"` with no `cpus` or `ram_mib`, so the
//!   Windows 11 defaults (2 vCPUs, 4096 MiB) apply. The first attempt must be
//!   `PciRoot(0x0)/Pci(0x2,0x0)`, and the host log must show the boot order
//!   Ternvale offered.
//!
//! Both must then reach the ISO's GRUB menu; the VM is cancelled there (with
//! ACPI the DSDT has no UART, so the Linux that GRUB starts prints nothing).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use ternvale_config::{Profile, VmConfig, WINDOWS_MIN_CPUS, WINDOWS_MIN_RAM_MIB};
use ternvale_devices::{attach_disks_pci, last_lines, Pl011};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{
    assets_root, drive, init_logging, log_dir, restore_stdin, stdin_pipe, write_result,
};
use crate::firmware::expect;

const SYSTEM_DISK_BYTES: u64 = ternvale_config::WINDOWS_MIN_DISK_BYTES;

pub fn run() -> Result<(), String> {
    let iso = assets_root().join("installer/installer.iso");
    if !iso.is_file() {
        return Err(format!(
            "missing {} (run ./scripts/fetch-installer-iso.sh)",
            iso.display()
        ));
    }
    let log_dir = log_dir();
    let guard = init_logging("windows-profile", &log_dir)?;
    let host = guard.log_path().to_path_buf();
    let system = log_dir.join("system.img");
    let file = std::fs::File::create(&system)
        .map_err(|err| format!("create {}: {err}", system.display()))?;
    file.set_len(SYSTEM_DISK_BYTES)
        .map_err(|err| format!("size {}: {err}", system.display()))?;
    drop(file);
    let started = Instant::now();
    let control = boot("control", &log_dir, &system, &iso, &host)?;
    check_first_load("control", &control, "PciRoot(0x0)/Pci(0x1,0x0)")?;
    let windows = boot("windows", &log_dir, &system, &iso, &host)?;
    check_first_load("windows", &windows, "PciRoot(0x0)/Pci(0x2,0x0)")?;
    check_host_log(&host)?;
    if let Err(error) = std::fs::remove_file(&system) {
        tracing::warn!(target: "ternvale::boot", path = %system.display(), error = %error, "could not remove the sparse system disk");
    }
    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        scenario = "windows-profile",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

fn config_text(label: &str, log_dir: &Path, system: &Path, iso: &Path) -> String {
    let preset = if label == "windows" {
        "profile = \"windows\"\n"
    } else {
        "cpus = 2\nram_mib = 4096\n"
    };
    let tpm = if label == "windows" {
        ""
    } else {
        "[tpm]\nenabled = false\n"
    };
    format!(
        "name = \"win-profile-{label}\"\n{preset}firmware = \"{firmware}\"\nnvram = \"{nvram}\"\n\
         serial_log = \"{serial}\"\n\n[[disks]]\npath = \"{system}\"\nread_only = false\n\n\
         [[disks]]\npath = \"{iso}\"\nread_only = true\n\n{tpm}",
        firmware = crate::common::firmware().display(),
        nvram = log_dir.join(format!("nvram-{label}.fd")).display(),
        serial = log_dir.join(format!("guest-serial-{label}.log")).display(),
        system = system.display(),
        iso = iso.display(),
    )
}

/// Boot `label`'s config to the GRUB menu and return its serial transcript.
fn boot(
    label: &str,
    log_dir: &Path,
    system: &Path,
    iso: &Path,
    host_log: &Path,
) -> Result<String, String> {
    let text = config_text(label, log_dir, system, iso);
    std::fs::write(log_dir.join(format!("{label}.toml")), &text)
        .map_err(|err| format!("write {label}.toml: {err}"))?;
    let vm = VmConfig::from_toml(&text).map_err(|err| format!("{label} config: {err}"))?;
    if label == "windows" {
        let want = (Profile::Windows, WINDOWS_MIN_CPUS, WINDOWS_MIN_RAM_MIB);
        if (vm.profile, vm.cpus, vm.ram_mib) != want {
            return Err(format!(
                "windows defaults: got {:?}/{} cpus/{} MiB, want {want:?}",
                vm.profile, vm.cpus, vm.ram_mib
            ));
        }
    }
    tracing::info!(target: "ternvale::boot", label, profile = %vm.profile, cpus = vm.cpus, ram_mib = vm.ram_mib, transport = %vm.virtio_transport(), "windows profile scenario boot");
    let serial_log = vm.serial_log.clone();
    let uart = Pl011::open(&serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let feed_log = serial_log.clone();
    let host = host_log.to_path_buf();
    let steps = vec![
        expect("UEFI firmware", 60),
        expect("BdsDxe: loading", 120),
        expect("GNU GRUB", 120),
    ];
    let feeder = std::thread::spawn(move || {
        let result = drive(&feed_log, &mut input, &flag, &host, steps);
        flag.store(true, Ordering::Release);
        result
    });
    let disks: Vec<(PathBuf, bool)> = vm
        .disks
        .iter()
        .map(|disk| (disk.path.clone(), disk.read_only))
        .collect();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        attach_disks_pci(attach, &disks).map(|_| Vec::new())
    });
    cancel.store(true, Ordering::Release);
    let script = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(log_dir, &exit, &script, &serial);
    script?;
    match exit {
        Ok(ExitReason::Canceled) => Ok(serial),
        other => Err(format!(
            "{label}: exit={other:?} (expected Canceled at the GRUB menu)\nlog={}\n--- serial ---\n{}",
            host_log.display(),
            last_lines(&serial, 30)
        )),
    }
}

/// BdsDxe prints `loading Boot#### "..." from <path>` for a loadable option
/// and `failed to load Boot#### "..." from <path>: Not Found` for a disk with
/// no boot loader (the blank system disk); either is an attempt.
fn check_first_load(label: &str, serial: &str, device: &str) -> Result<(), String> {
    let attempts: Vec<&str> = serial
        .lines()
        .filter_map(|line| {
            ["BdsDxe: loading", "BdsDxe: failed to load"]
                .iter()
                .find_map(|tag| line.find(tag))
                .map(|at| line[at..].trim_end())
        })
        .collect();
    tracing::info!(target: "ternvale::boot", label, attempts = %attempts.join(" | "), "uefi boot attempts");
    let from = format!("from {device}");
    match attempts.first() {
        Some(first) if first.contains(&from) => Ok(()),
        first => Err(format!(
            "{label}: first BdsDxe boot attempt was {first:?}, want one {from}"
        )),
    }
}

fn check_host_log(host_log: &Path) -> Result<(), String> {
    let log = std::fs::read_to_string(host_log)
        .map_err(|err| format!("read {}: {err}", host_log.display()))?;
    let want = "/pci@i0cf8/scsi@2/disk@0,0 /pci@i0cf8/scsi@1/disk@0,0";
    let offered = log
        .lines()
        .any(|line| line.contains("uefi boot order: attached ISO first") && line.contains(want));
    let on_fw_cfg = log
        .lines()
        .any(|line| line.contains("fw_cfg ready") && line.contains("bootorder"));
    if offered && on_fw_cfg {
        return Ok(());
    }
    Err(format!(
        "host log {}: boot order offered={offered} (want {want:?}), on fw_cfg={on_fw_cfg}",
        host_log.display()
    ))
}
