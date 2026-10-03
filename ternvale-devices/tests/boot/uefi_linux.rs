//! UEFI Linux scenario: the harness test kernel (`test-assets/Image`, an
//! arm64 Image with the EFI stub) and `initramfs.cpio` booted through EDK2
//! with `firmware_tables = "fdt"`.
//!
//! The two files go on a FAT16 image (`fat.rs`) attached as a read-only
//! virtio-mmio disk. In the UEFI shell, `fs0:\IMAGE.EFI initrd=\INITRD …`
//! starts the EFI stub, which loads the initrd from the same volume and
//! must report `Using DTB from configuration table`. The
//! Step 18 checks then run: `Linux version`, the `# ` prompt, `uname -a` with
//! `aarch64`, `echo OK`, and `poweroff -f` ending in PSCI `SYSTEM_OFF`. The
//! kernel must also report that it came up through EFI with the device tree:
//! `/sys/firmware/efi` and `/sys/firmware/fdt` exist, `/sys/firmware/acpi`
//! does not.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_config::{Disk, FirmwareTables, VmConfig};
use ternvale_devices::{attach_disks, last_lines, Pl011, Step};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{
    assets_root, drive, init_logging, log_dir, restore_stdin, stdin_pipe, write_result,
};
use crate::firmware::{check_banner, expect, send, to_shell};

const KERNEL: &str = "IMAGE.EFI";
const INITRD: &str = "INITRD";

pub fn run() -> Result<(), String> {
    let log_dir = log_dir();
    let guard = init_logging("uefi-linux", &log_dir)?;
    let host = guard.log_path().to_path_buf();
    let serial_log = log_dir.join("guest-serial.log");
    let nvram = log_dir.join("nvram.fd");
    let esp = log_dir.join("esp.img");
    let root = assets_root();
    let kernel = read(&root.join("Image"))?;
    let initrd = read(&root.join("initramfs.cpio"))?;
    crate::fat::write_fat16(&esp, &[(KERNEL, &kernel), (INITRD, &initrd)])?;
    let started = Instant::now();
    boot(&esp, &log_dir, &serial_log, &nvram, &host)?;
    check_banner(&serial_log)?;
    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        scenario = "uefi-linux",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|err| format!("read {}: {err}", path.display()))
}

fn boot(
    esp: &Path,
    log_dir: &Path,
    serial_log: &Path,
    nvram: &Path,
    host_log: &Path,
) -> Result<(), String> {
    let vm = VmConfig {
        name: "uefi-linux".to_string(),
        cpus: 1,
        ram_mib: 512,
        kernel: Default::default(),
        initrd: None,
        cmdline: String::new(),
        boot_disk: false,
        disks: vec![Disk {
            path: esp.to_path_buf(),
            read_only: true,
        }],
        nics: Vec::new(),
        serial_log: serial_log.to_path_buf(),
        firmware: Some(crate::common::firmware()),
        nvram: Some(nvram.to_path_buf()),
        firmware_tables: Some(FirmwareTables::Fdt),
        vsock: None,
    };
    tracing::info!(target: "ternvale::boot", esp = %esp.display(), serial = %serial_log.display(), "uefi linux boot");
    let uart = Pl011::open(serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let feed_log = serial_log.to_path_buf();
    let host = host_log.to_path_buf();
    let feeder = std::thread::spawn(move || drive(&feed_log, &mut input, &flag, &host, steps()));
    let disks: Vec<(PathBuf, bool)> = vec![(esp.to_path_buf(), true)];
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        attach_disks(attach, &disks)
    });
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

/// `text` in 16-byte chunks (the PL011 RX FIFO), then `end`.
fn line(text: &str, end: &[u8]) -> Vec<Step> {
    let mut chunks: Vec<&[u8]> = text.as_bytes().chunks(16).collect();
    chunks.push(end);
    send(&chunks)
}

/// A busybox shell command whose output must contain `want`. The commands
/// split markers with `""`, so the echoed command line cannot match.
fn check(command: &str, want: &str) -> Vec<Step> {
    let mut steps = line(command, b"\n");
    steps.push(expect(want, 15));
    steps
}

fn steps() -> Vec<Step> {
    let mut steps = to_shell(true);
    let command = format!(
        "fs0:\\{KERNEL} initrd=\\{INITRD} {}",
        ternvale_vmm::DEFAULT_CMDLINE
    );
    steps.extend(line(&command, b"\r"));
    steps.push(expect("EFI stub: Booting Linux Kernel", 120));
    steps.push(expect("EFI stub: Using DTB from configuration table", 60));
    steps.push(expect("Linux version", 60));
    steps.push(expect("Machine model: ternvale,virt", 60));
    steps.push(expect("# ", 60));
    steps.extend(check("/bin/busybox mkdir -p /sys; echo MK\"\"_OK", "MK_OK"));
    steps.extend(check(
        "/bin/busybox mount -t sysfs sysfs /sys && echo SYS\"\"_OK",
        "SYS_OK",
    ));
    steps.extend(check(
        "[ -d /sys/firmware/efi ] && echo EFI\"\"_YES",
        "EFI_YES",
    ));
    steps.extend(check(
        "[ -e /sys/firmware/fdt ] && echo FDT\"\"_YES",
        "FDT_YES",
    ));
    steps.extend(check(
        "[ -e /sys/firmware/acpi ] || echo ACPI\"\"_NO",
        "ACPI_NO",
    ));
    steps.extend(check("/bin/busybox uname -a", "aarch64"));
    steps.extend(check("echo OK", "\nOK"));
    steps.extend(line("/bin/busybox poweroff -f", b"\n"));
    steps.push(Step::Expect {
        pattern: "reboot: Power down".to_string(),
        timeout: Duration::from_secs(30),
    });
    steps
}
