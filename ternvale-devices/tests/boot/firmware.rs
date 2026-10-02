//! UEFI scenario: boot EDK2 ArmVirtQemu (`test-assets/firmware/QEMU_EFI.fd`)
//! twice on one NVRAM file.
//!
//! Boot 1 starts from a missing NVRAM file, reaches the UEFI shell, stores a
//! non-volatile variable, leaves the shell (BDS then shows the UiApp front
//! page), opens Boot Manager, boots "EFI Internal Shell" from it, and powers
//! off with `reset -s` (PSCI `SYSTEM_OFF`). Boot 2 reads the variable back.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_devices::{Pl011, Step};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{
    assets_root, drive, init_logging, log_dir, restore_stdin, stdin_pipe, write_result,
};

/// EDK2's variable store: variables + FTW working + FTW spare, 256 KiB each.
const VARSTORE_BYTES: u64 = 0xc_0000;
/// Printed by ArmVirtQemu's PlatformBootManagerLib before BDS boots anything.
const BANNER: &str = "UEFI firmware (version";
const GUID: [&[u8]; 3] = [b"9f1e8a32-2b3c-", b"4d5e-8f60-11223", b"3445566"];

pub fn run() -> Result<(), String> {
    let log_dir = log_dir();
    let guard = init_logging("firmware", &log_dir)?;
    let host = guard.log_path().to_path_buf();
    let nvram = log_dir.join("nvram.fd");
    if nvram.exists() {
        std::fs::remove_file(&nvram).map_err(|err| format!("remove old nvram: {err}"))?;
    }
    let started = Instant::now();
    let first = log_dir.join("guest-serial-1.log");
    boot_once(&log_dir, &first, &nvram, &host, first_boot())?;
    check_banner(&first)?;
    check_nvram(&nvram)?;
    let second = log_dir.join("guest-serial-2.log");
    boot_once(&log_dir, &second, &nvram, &host, second_boot())?;
    check_banner(&second)?;
    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        scenario = "firmware",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

fn boot_once(
    log_dir: &Path,
    serial_log: &Path,
    nvram: &Path,
    host_log: &Path,
    steps: Vec<Step>,
) -> Result<(), String> {
    let vm = VmConfig {
        name: "uefi".to_string(),
        cpus: 1,
        ram_mib: 512,
        kernel: Default::default(),
        initrd: None,
        cmdline: String::new(),
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: serial_log.to_path_buf(),
        firmware: Some(assets_root().join("firmware/QEMU_EFI.fd")),
        nvram: Some(nvram.to_path_buf()),
        vsock: None,
    };
    tracing::info!(target: "ternvale::boot", serial = %serial_log.display(), nvram = %nvram.display(), "firmware boot");
    let uart = Pl011::open(serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let feed_log = serial_log.to_path_buf();
    let host = host_log.to_path_buf();
    let feeder = std::thread::spawn(move || drive(&feed_log, &mut input, &flag, &host, steps));
    let exit = Machine::run_until(&vm, Box::new(uart), Arc::clone(&cancel));
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
            ternvale_devices::last_lines(&serial, 20)
        )),
    }
}

/// The saved serial log holds the EDK2 banner line, e.g.
/// `UEFI firmware (version  built at 07:52:30 on May  6 2024)`.
pub fn check_banner(serial_log: &Path) -> Result<(), String> {
    let serial = std::fs::read_to_string(serial_log)
        .map_err(|err| format!("read {}: {err}", serial_log.display()))?;
    let Some(line) = serial.lines().find(|line| line.contains(BANNER)) else {
        tracing::error!(
            target: "ternvale::boot",
            serial = %serial_log.display(),
            bytes = serial.len(),
            "UEFI banner missing from serial log"
        );
        return Err(format!(
            "{} has no {BANNER:?} line\n--- serial ---\n{}",
            serial_log.display(),
            ternvale_devices::last_lines(&serial, 20)
        ));
    };
    let start = line.find(BANNER).unwrap_or(0);
    tracing::info!(
        target: "ternvale::boot",
        serial = %serial_log.display(),
        banner = %line[start..].trim_end(),
        "UEFI banner present in serial log"
    );
    Ok(())
}

/// EDK2 formatted the blank store: full size, `_FVH` signature at 0x28.
fn check_nvram(nvram: &Path) -> Result<(), String> {
    let bytes = std::fs::read(nvram).map_err(|err| format!("read nvram: {err}"))?;
    let signature = bytes.get(0x28..0x2c);
    tracing::info!(
        target: "ternvale::boot",
        bytes = bytes.len(),
        signature = ?signature.map(String::from_utf8_lossy),
        "nvram after first boot"
    );
    if bytes.len() as u64 != VARSTORE_BYTES || signature != Some(b"_FVH".as_slice()) {
        return Err(format!(
            "nvram is {:#x} bytes with signature {signature:?}; expected {VARSTORE_BYTES:#x} and _FVH",
            bytes.len()
        ));
    }
    Ok(())
}

fn expect(pattern: &str, secs: u64) -> Step {
    Step::Expect {
        pattern: pattern.to_string(),
        timeout: Duration::from_secs(secs),
    }
}

/// Each chunk stays within the PL011's 16-byte RX FIFO.
fn send(chunks: &[&[u8]]) -> Vec<Step> {
    chunks
        .iter()
        .map(|chunk| Step::Send {
            data: chunk.to_vec(),
        })
        .collect()
}

/// Banner, then skip the shell's 5-second `startup.nsh` countdown.
fn to_shell(banner: bool) -> Vec<Step> {
    let mut steps = Vec::new();
    if banner {
        steps.push(expect("UEFI firmware", 60));
    }
    steps.push(expect("any other key", 60));
    steps.extend(send(&[b" "]));
    steps.push(expect("Shell>", 30));
    steps
}

fn dump_mark() -> Vec<Step> {
    let mut chunks: Vec<&[u8]> = vec![b"dmpstore TvMark", b" -guid "];
    chunks.extend(GUID);
    chunks.push(b"\r");
    let mut steps = send(&chunks);
    steps.push(expect("*persist*", 30));
    steps.push(expect("Shell>", 30));
    steps
}

fn power_off() -> Vec<Step> {
    send(&[b"reset -s\r"])
}

fn first_boot() -> Vec<Step> {
    let mut steps = to_shell(true);
    let mut chunks: Vec<&[u8]> = vec![b"setvar TvMark ", b"-nv -bs -guid "];
    chunks.extend(GUID);
    chunks.extend([&b" =\"persist\"\r"[..]]);
    steps.extend(send(&chunks));
    steps.push(expect("Shell>", 30));
    steps.extend(dump_mark());
    // Leaving the shell with EFI_SUCCESS makes BDS show the boot manager menu
    // app (UiApp, the front page).
    steps.extend(send(&[b"exit\r"]));
    steps.push(expect("UiApp", 30));
    steps.push(expect("Device Manager", 30));
    steps.push(expect("Reset", 30));
    // Select Language, Device Manager, Boot Manager.
    steps.extend(send(&[b"\x1b[B", b"\x1b[B", b"\r"]));
    steps.push(expect("Boot Manager Menu", 30));
    steps.push(expect("EFI Internal Shell", 30));
    steps.extend(send(&[b"\r"]));
    steps.extend(to_shell(false));
    steps.extend(power_off());
    steps
}

fn second_boot() -> Vec<Step> {
    let mut steps = to_shell(true);
    steps.extend(dump_mark());
    steps.extend(power_off());
    steps
}
