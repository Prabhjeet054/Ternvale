//! `lspci` cross-check: the same Linux, through the same EDK2, with the same
//! virtio-pci disks, booted once with `firmware_tables = "fdt"` (DTB, the
//! Linux default) and once with `"acpi"` (MCFG + DSDT `\_SB.PCI0`). Both boots
//! must enumerate the same PCI functions and bind the same drivers.
//!
//! Disks, all read-only virtio-pci: 00:01.0 the ESP (`IMAGE.EFI`, `INITRD`),
//! 00:02.0 `rootfs.ext4` (`pciutils` from `./scripts/make-pci-assets.sh`),
//! 00:03.0 `data.ext4`. `INITRD` is the `make-rootfs.sh` initramfs with `/init`
//! replaced (see [`INIT`]): it mounts the rootfs read-only and runs its real
//! `lspci -nnk`. With ACPI the DSDT has no UART device, so there is no
//! console; the script therefore writes everything to `/dev/kmsg`, which
//! reaches the serial log through earlycon (acpi) or ttyAMA0 (fdt) alike,
//! then powers off. The acpi boot also dumps the DSDT Linux loaded, which
//! must be the builder's bytes (the ones the iasl tests disassemble).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use ternvale_config::{Disk, FirmwareTables, VmConfig};
use ternvale_devices::{attach_disks_pci, last_lines, Pl011, Step};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{
    assets_root, drive, init_logging, log_dir, restore_stdin, stdin_pipe, write_result,
};
use crate::firmware::{check_banner, expect, to_shell};
use crate::uefi_linux::{line, INITRD, KERNEL};

const CPUS: u32 = 2;
const LSPCI: &str = "ternvale-lspci: ";
const DSDT: &str = "ternvale-dsdt: ";
const DONE: &str = "ternvale-xc: done";
/// Functions both boots must list: the host bridge and three virtio-blk disks.
const FUNCTIONS: [&str; 4] = [
    "00:00.0 Host bridge [0600]: ",
    "00:01.0 SCSI storage controller [0100]: ",
    "00:02.0 SCSI storage controller [0100]: ",
    "00:03.0 SCSI storage controller [0100]: ",
];

/// Replacement `/init`. Runs with no console under ACPI, so stdout and
/// stderr go to `/dev/kmsg` (`printk.devkmsg=on` lifts its rate limit).
const INIT: &str = r#"#!/bin/busybox sh
B=/bin/busybox
$B mkdir -p /proc /sys /dev /newroot
$B mount -t proc proc /proc
$B mount -t sysfs sys /sys
$B mount -t devtmpfs dev /dev
exec </dev/null >/dev/kmsg 2>&1
say() { $B echo "ternvale-xc: $*"; }
for m in crc16 crc32c_generic libcrc32c mbcache jbd2 ext4 virtio_blk; do
  $B insmod /modules/$m.ko || say "insmod $m failed"
done
i=0
while [ ! -b /dev/vdc ] && [ "$i" -lt 100 ]; do $B sleep 0.1; i=$((i + 1)); done
say "efi=$([ -d /sys/firmware/efi ] && echo y || echo n) acpi=$([ -d /sys/firmware/acpi/tables ] && echo y || echo n) dt_pcie=$([ -d /sys/firmware/devicetree/base/pcie@3f000000 ] && echo y || echo n)"
if $B mount -t ext4 -o ro,noload /dev/vdb /newroot && $B mount -t sysfs sys /newroot/sys; then
  $B chroot /newroot /usr/bin/lspci -nnk >/lspci.txt 2>&1
  say "lspci exit $?"
  while IFS= read -r l; do $B echo "ternvale-lspci: $l"; done </lspci.txt
else
  say "rootfs mount failed"
fi
if [ -r /sys/firmware/acpi/tables/DSDT ]; then
  $B od -An -v -tx1 /sys/firmware/acpi/tables/DSDT | while read -r l; do $B echo "ternvale-dsdt: $l"; done
fi
say done
$B sync
$B poweroff -f
"#;

/// What one boot printed.
struct Seen {
    lspci: Vec<String>,
    probe: Vec<String>,
    dsdt: Vec<u8>,
}

pub fn run() -> Result<(), String> {
    let log_dir = log_dir();
    let guard = init_logging("uefi-lspci", &log_dir)?;
    let host = guard.log_path().to_path_buf();
    let root = assets_root().join("virtio-root");
    let read = |name: &str| {
        let path = root.join(name);
        std::fs::read(&path).map_err(|err| {
            format!(
                "read {}: {err} (run ./scripts/make-rootfs.sh and ./scripts/make-pci-assets.sh)",
                path.display()
            )
        })
    };
    let kernel = read("Image")?;
    let initrd = crate::cpio::append(
        &read("initramfs.cpio")?,
        &[("init", 0o755, INIT.as_bytes())],
    )?;
    let esp = log_dir.join("esp.img");
    crate::fat::write_fat16_mbr(&esp, &[(KERNEL, &kernel), (INITRD, &initrd)])?;
    let disks = vec![esp, root.join("rootfs.ext4"), root.join("data.ext4")];
    let started = Instant::now();
    let fdt = boot(FirmwareTables::Fdt, &disks, &log_dir, &host)?;
    let acpi = boot(FirmwareTables::Acpi, &disks, &log_dir, &host)?;
    compare(&fdt, &acpi)?;
    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        functions = fdt.lspci.iter().filter(|l| !l.starts_with('\t')).count(),
        lspci_lines = fdt.lspci.len(),
        dsdt_bytes = acpi.dsdt.len(),
        scenario = "uefi-lspci",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

fn boot(
    tables: FirmwareTables,
    disks: &[PathBuf],
    log_dir: &Path,
    host_log: &Path,
) -> Result<Seen, String> {
    let label = match tables {
        FirmwareTables::Fdt => "fdt",
        FirmwareTables::Acpi => "acpi",
    };
    let serial_log = log_dir.join(format!("guest-serial-{label}.log"));
    let vm = VmConfig {
        name: format!("uefi-lspci-{label}"),
        cpus: CPUS,
        ram_mib: 512,
        kernel: Default::default(),
        initrd: None,
        cmdline: String::new(),
        boot_disk: false,
        disks: disks
            .iter()
            .map(|path| Disk {
                path: path.clone(),
                read_only: true,
            })
            .collect(),
        nics: Vec::new(),
        serial_log: serial_log.clone(),
        firmware: Some(crate::common::firmware()),
        nvram: Some(log_dir.join(format!("nvram-{label}.fd"))),
        firmware_tables: Some(tables),
        vsock: None,
    };
    tracing::info!(target: "ternvale::boot", tables = label, serial = %serial_log.display(), "uefi lspci boot");
    let uart = Pl011::open(&serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let feed_log = serial_log.clone();
    let host = host_log.to_path_buf();
    let feeder = std::thread::spawn(move || drive(&feed_log, &mut input, &flag, &host, steps()));
    let attach: Vec<(PathBuf, bool)> = disks.iter().map(|p| (p.clone(), true)).collect();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |a| {
        attach_disks_pci(a, &attach).map(|_| Vec::new())
    });
    cancel.store(true, Ordering::Release);
    let script = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(log_dir, &exit, &script, &serial);
    script?;
    if !matches!(exit, Ok(ExitReason::SystemOff)) {
        return Err(format!(
            "{label}: exit={exit:?} (expected SystemOff)\nlog={}\n--- serial ---\n{}",
            host_log.display(),
            last_lines(&serial, 30)
        ));
    }
    check_banner(&serial_log)?;
    let seen = parse(&serial);
    check_one(label, &serial, &seen)?;
    Ok(seen)
}

fn steps() -> Vec<Step> {
    let mut steps = to_shell(true);
    let command = format!(
        "fs0:\\{KERNEL} initrd=\\{INITRD} {} printk.devkmsg=on",
        ternvale_vmm::DEFAULT_CMDLINE
    );
    steps.extend(line(&command, b"\r"));
    steps.push(expect("EFI stub: Booting Linux Kernel", 120));
    steps.push(expect(DONE, 90));
    steps
}

/// Text after `marker` on each serial line that has it, in order.
fn after<'a>(serial: &'a str, marker: &str) -> Vec<&'a str> {
    serial
        .lines()
        .filter_map(|l| l.split_once(marker).map(|(_, rest)| rest.trim_end()))
        .collect()
}

fn parse(serial: &str) -> Seen {
    let lspci = after(serial, LSPCI)
        .into_iter()
        .map(str::to_string)
        .collect();
    let probe = serial
        .lines()
        .filter(|l| l.contains("] type 00 class ") || l.contains("] type 01 class "))
        .filter_map(|l| l.find("pci 0000:").map(|at| l[at..].trim_end().to_string()))
        .collect();
    let dsdt = after(serial, DSDT)
        .into_iter()
        .flat_map(|l| l.split_whitespace())
        .filter_map(|byte| u8::from_str_radix(byte, 16).ok())
        .collect();
    Seen { lspci, probe, dsdt }
}

/// Per-boot checks: the firmware path really was `label` (under ACPI the EFI
/// stub still passes an empty DTB, so `/sys/firmware/fdt` exists in both;
/// only the real DTB has the `pcie@3f000000` node), lspci ran, and every
/// expected function is there with `virtio-pci` bound to each disk.
fn check_one(label: &str, serial: &str, seen: &Seen) -> Result<(), String> {
    let path_marker = match label {
        "fdt" => "ternvale-xc: efi=y acpi=n dt_pcie=y",
        _ => "ternvale-xc: efi=y acpi=y dt_pcie=n",
    };
    let mut problems = Vec::new();
    for want in [path_marker, "ternvale-xc: lspci exit 0"] {
        if !serial.contains(want) {
            problems.push(format!("missing {want:?}"));
        }
    }
    for function in FUNCTIONS {
        if !seen.lspci.iter().any(|l| l.starts_with(function)) {
            problems.push(format!("lspci has no {function:?}"));
        }
    }
    let drivers = seen
        .lspci
        .iter()
        .filter(|l| l.trim() == "Kernel driver in use: virtio-pci")
        .count();
    if drivers != 3 {
        problems.push(format!("{drivers} functions bound to virtio-pci, want 3"));
    }
    for line in &seen.lspci {
        tracing::info!(target: "ternvale::boot", tables = label, line = %line, "guest lspci");
    }
    if problems.is_empty() {
        return Ok(());
    }
    tracing::error!(target: "ternvale::boot", tables = label, problems = %problems.join("; "), "lspci boot check failed");
    Err(format!("{label}: {}", problems.join("; ")))
}

/// Both boots must agree on lspci and on the kernel's own probe lines, and the
/// DSDT Linux loaded under ACPI must be the builder's.
fn compare(fdt: &Seen, acpi: &Seen) -> Result<(), String> {
    let mut problems = Vec::new();
    if fdt.lspci != acpi.lspci {
        problems.push(format!(
            "lspci differs:\n--- fdt ---\n{}\n--- acpi ---\n{}",
            fdt.lspci.join("\n"),
            acpi.lspci.join("\n")
        ));
    }
    if fdt.probe != acpi.probe {
        problems.push(format!(
            "kernel probe differs:\n--- fdt ---\n{}\n--- acpi ---\n{}",
            fdt.probe.join("\n"),
            acpi.probe.join("\n")
        ));
    }
    if !fdt.dsdt.is_empty() {
        problems.push(format!("fdt boot exposed a {}-byte DSDT", fdt.dsdt.len()));
    }
    let built = ternvale_acpi::dsdt(&ternvale_vmm::acpi_config(CPUS))
        .map_err(|err| format!("build dsdt: {err}"))?;
    if acpi.dsdt != built {
        problems.push(format!(
            "guest DSDT is {} bytes, builder's is {} bytes, first difference at {:?}",
            acpi.dsdt.len(),
            built.len(),
            acpi.dsdt.iter().zip(&built).position(|(a, b)| a != b)
        ));
    }
    tracing::info!(
        target: "ternvale::boot",
        lspci_equal = fdt.lspci == acpi.lspci,
        probe_equal = fdt.probe == acpi.probe,
        probe_lines = fdt.probe.len(),
        guest_dsdt_matches_builder = acpi.dsdt == built,
        "fdt vs acpi pci cross-check"
    );
    if problems.is_empty() {
        return Ok(());
    }
    tracing::error!(target: "ternvale::boot", problems = %problems.join(" | "), "fdt vs acpi pci cross-check failed");
    Err(problems.join("\n"))
}
