//! Installer scenario: EDK2 boots the Alpine virt arm64 ISO
//! (`./scripts/fetch-installer-iso.sh`) from a read-only virtio-blk disk.
//!
//! UEFI BDS finds the removable-media loader `\EFI\BOOT\BOOTAA64.EFI` (GRUB)
//! in the ISO's El Torito EFI image; GRUB loads `vmlinuz-virt` through the
//! Linux EFI stub; the live system reaches a root login, where `setup-alpine`
//! (the installer) is started and then abandoned before powering off.
//!
//! `TERNVALE_INSTALLER_TRANSPORT=pci` attaches the ISO as virtio-pci 00:01.0
//! instead of virtio-mmio slot 0.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_config::{Disk, VmConfig};
use ternvale_devices::{attach_disks, attach_disks_pci, last_lines, Pl011, Step};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{
    assets_root, drive, init_logging, log_dir, restore_stdin, stdin_pipe, write_result,
};
use crate::firmware::check_banner;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Mmio,
    Pci,
}

fn transport() -> Result<Transport, String> {
    match std::env::var("TERNVALE_INSTALLER_TRANSPORT").as_deref() {
        Err(_) | Ok("") | Ok("mmio") => Ok(Transport::Mmio),
        Ok("pci") => Ok(Transport::Pci),
        Ok(other) => Err(format!(
            "TERNVALE_INSTALLER_TRANSPORT={other:?} (expected mmio or pci)"
        )),
    }
}

pub fn run() -> Result<(), String> {
    let iso = assets_root().join("installer/installer.iso");
    if !iso.is_file() {
        return Err(format!(
            "missing {} (run ./scripts/fetch-installer-iso.sh)",
            iso.display()
        ));
    }
    let transport = transport()?;
    let log_dir = log_dir();
    let guard = init_logging("installer", &log_dir)?;
    let host = guard.log_path().to_path_buf();
    let serial_log = log_dir.join("guest-serial.log");
    let nvram = log_dir.join("nvram.fd");
    tracing::info!(
        target: "ternvale::boot",
        iso = %iso.display(),
        transport = ?transport,
        serial = %serial_log.display(),
        "installer boot"
    );
    let started = Instant::now();
    let result = boot(&iso, transport, &log_dir, &serial_log, &nvram, &host);
    let banner = check_banner(&serial_log);
    result?;
    banner?;
    save_fdt(&serial_log, &log_dir)?;
    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        scenario = "installer",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

fn boot(
    iso: &Path,
    transport: Transport,
    log_dir: &Path,
    serial_log: &Path,
    nvram: &Path,
    host_log: &Path,
) -> Result<(), String> {
    let vm = VmConfig {
        name: "installer".to_string(),
        cpus: 1,
        ram_mib: 1024,
        kernel: Default::default(),
        initrd: None,
        cmdline: String::new(),
        boot_disk: false,
        disks: vec![Disk {
            path: iso.to_path_buf(),
            read_only: true,
        }],
        nics: Vec::new(),
        serial_log: serial_log.to_path_buf(),
        firmware: Some(crate::common::firmware()),
        nvram: Some(nvram.to_path_buf()),
        firmware_tables: Some(ternvale_config::FirmwareTables::Fdt),
        vsock: None,
    };
    let uart = Pl011::open(serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let feed_log = serial_log.to_path_buf();
    let host = host_log.to_path_buf();
    let feeder = std::thread::spawn(move || drive(&feed_log, &mut input, &flag, &host, steps()));
    let disks: Vec<(PathBuf, bool)> = vec![(iso.to_path_buf(), true)];
    let exit =
        Machine::run_with(
            &vm,
            Box::new(uart),
            Arc::clone(&cancel),
            move |attach| match transport {
                Transport::Mmio => attach_disks(attach, &disks),
                Transport::Pci => attach_disks_pci(attach, &disks).map(|_| Vec::new()),
            },
        );
    cancel.store(true, Ordering::Release);
    let script = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let serial = std::fs::read_to_string(serial_log).unwrap_or_default();
    write_result(log_dir, &exit, &script, &serial);
    script?;
    match exit {
        Ok(ExitReason::SystemOff) => Ok(()),
        other => Err(format!(
            "exit={other:?}\nlog={}\n--- serial ---\n{}",
            host_log.display(),
            last_lines(&serial, 30)
        )),
    }
}

fn expect(pattern: &str, secs: u64) -> Step {
    Step::Expect {
        pattern: pattern.to_string(),
        timeout: Duration::from_secs(secs),
    }
}

fn send(chunk: &[u8]) -> Step {
    Step::Send {
        data: chunk.to_vec(),
    }
}

/// A shell line split into the PL011's 16-byte RX FIFO, then CR.
fn line(text: &str) -> Vec<Step> {
    let mut steps: Vec<Step> = text.as_bytes().chunks(16).map(send).collect();
    steps.push(send(b"\r"));
    steps
}

/// Run `text`, then wait for `TV-<n>`; the shell echo shows `$((n-1+1))`,
/// so only the command's completion matches.
fn run_marked(text: &str, n: u32, secs: u64) -> Vec<Step> {
    let mut steps = line(&format!("{text}; echo TV-$(({}+1))", n - 1));
    steps.push(expect(&format!("TV-{n}"), secs));
    steps
}

/// Firmware-dependent facts the guest saw, for comparison with QEMU.
fn diagnostics() -> Vec<Step> {
    let mut steps = Vec::new();
    steps.extend(run_marked(
        "dmesg | grep -iE 'efi|dmi|smbios|acpi|rtc|psci|rng|magic|fail|error|warn|Machine'",
        1,
        30,
    ));
    steps.extend(run_marked("ls /sys/firmware /sys/firmware/efi", 2, 30));
    steps.extend(run_marked(
        "ls /sys/firmware/efi/efivars | cut -d- -f1 | sort | tr '\\n' ' '",
        3,
        30,
    ));
    steps.extend(run_marked("ls /sys/bus/platform/devices", 4, 30));
    steps.extend(run_marked("cat /proc/interrupts; blkid", 5, 30));
    steps.extend(run_marked(FDT_DUMP, FDT_MARK, 60));
    steps
}

/// The DT EDK2 handed to Linux (EDK2 edits the VMM's tree, e.g. disables
/// the PL031 it owns), as base64 between the command echo and `TV-6`.
const FDT_DUMP: &str = "base64 /sys/firmware/fdt";
const FDT_MARK: u32 = 6;

/// Save the guest's `/sys/firmware/fdt` dump as `guest-fdt.b64`
/// (`base64 -D -i guest-fdt.b64 | dtc -I dtb -O dts` decompiles it).
fn save_fdt(serial_log: &Path, log_dir: &Path) -> Result<(), String> {
    let serial = std::fs::read_to_string(serial_log)
        .map_err(|err| format!("read {}: {err}", serial_log.display()))?;
    let marker = format!("TV-{FDT_MARK}");
    let body: Vec<&str> = serial
        .lines()
        .skip_while(|line| !line.contains(FDT_DUMP))
        .skip(1)
        .map(|line| line.trim_end_matches('\r'))
        .take_while(|line| *line != marker)
        .filter(|line| {
            line.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
        })
        .collect();
    let path = log_dir.join("guest-fdt.b64");
    std::fs::write(&path, body.join("\n") + "\n")
        .map_err(|err| format!("write {}: {err}", path.display()))?;
    tracing::info!(
        target: "ternvale::boot",
        path = %path.display(),
        lines = body.len(),
        "saved guest /sys/firmware/fdt"
    );
    Ok(())
}

fn steps() -> Vec<Step> {
    let mut steps = vec![
        expect("UEFI firmware", 60),
        expect("UEFI Misc Device", 60),
        expect("GNU GRUB", 60),
        expect("Booting `Linux virt'", 30),
        expect("OpenRC", 120),
        expect("Welcome to Alpine Linux", 300),
        expect("login:", 60),
        send(b"root\r"),
        expect("localhost:~#", 60),
    ];
    steps.extend(diagnostics());
    steps.extend(line("setup-alpine"));
    steps.push(expect("ALPINE LINUX INSTALL", 60));
    steps.push(expect("Enter system hostname", 60));
    steps.push(send(b"\x03"));
    steps.push(expect("localhost:~#", 30));
    steps.extend(line("poweroff"));
    steps
}
