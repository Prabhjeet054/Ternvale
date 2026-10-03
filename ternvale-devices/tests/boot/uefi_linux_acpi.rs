//! UEFI Linux with ACPI scenario: the test kernel finds the virtio-pci disk
//! through the DSDT root bridge `\_SB.PCI0` and the MCFG alone.
//!
//! EDK2 boots with `firmware_tables = "acpi"`, so the kernel gets ACPI and an
//! empty DTB from the EFI stub. The ESP (`IMAGE.EFI`, `INITRD`) is an MBR
//! disk on virtio-pci 00:01.0, with no ACPI node of its own. It carries the
//! `./scripts/make-rootfs.sh` kernel and initramfs: `virtio_pci` is built in,
//! and that initramfs loads the matching `virtio_blk` module (it then fails to
//! mount the FAT disk as its root, which is fine here). The DSDT has no UART
//! device yet, so there is no console to type into: everything is read from
//! the earlycon log, and the VM is cancelled at the end.
//!
//! Required, in order: `ACPI: PCI Root Bridge [PCI0] (domain 0000 [bus
//! 00-0f])`, the ECAM found through the MCFG, the `_CRS` window and bus range
//! as root bus resources, the disk found by enumeration (`[1af4:1042]`), and
//! ` vda: vda1`: Linux read the MBR, so a request completed. MSI-X is a stub,
//! so virtio-pci requests the INTx line `_PRT` gave it; without a route Linux
//! warns `can't derive routing` and the probe fails. No ACPI BIOS error, no
//! AML exception, no missing route. (`_OSC ... (AE_NOT_FOUND)` is expected:
//! PCI0 has no `_OSC`.)
//! Linux prints the `PCI INT A -> GSI 36` line only with dynamic debug, so the
//! host log is checked instead: SPI 4 (device 1 INTA) must be deasserted,
//! which only a guest ISR read in the interrupt handler does.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use ternvale_config::{Disk, FirmwareTables, VmConfig};
use ternvale_devices::{attach_disks_pci, last_lines, Pl011, Step};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{
    assets_root, drive, init_logging_at, log_dir, restore_stdin, stdin_pipe, write_result,
};
use crate::firmware::{check_banner, expect, to_shell};
use crate::uefi_linux::{line, INITRD, KERNEL};

/// The disk sits at 00:01.0; INTA swizzles to line (1 + 0) % 4 = SPI 4.
const DISK_SPI: u32 = ternvale_vmm::pci::PCI_INTX_SPI0 + 1;
/// Fewest deasserts of the disk's line that show the guest handler ran. EDK2
/// polls the used ring and never toggles the line.
const MIN_DEASSERTS: usize = 1;

/// Lines that mean the namespace or the routing is wrong.
const FORBIDDEN: [&str; 6] = [
    "ACPI BIOS Error",
    "ACPI Error",
    "ACPI Exception",
    "can't derive routing",
    "no GSI",
    "pci_bus 0000:00: root bus resource [io",
];

pub fn run() -> Result<(), String> {
    let log_dir = log_dir();
    let level = std::env::var("TERNVALE_BOOT_LOG").unwrap_or_else(|_| "info".to_string());
    let guard = init_logging_at(
        "uefi-linux-acpi",
        &log_dir,
        &format!("{level},ternvale::pci=debug"),
    )?;
    let host = guard.log_path().to_path_buf();
    let serial_log = log_dir.join("guest-serial.log");
    let esp = log_dir.join("esp.img");
    let root = assets_root().join("virtio-root");
    let read = |name: &str| {
        let path = root.join(name);
        std::fs::read(&path).map_err(|err| format!("read {}: {err}", path.display()))
    };
    let (kernel, initrd) = (read("Image")?, read("initramfs.cpio")?);
    crate::fat::write_fat16_mbr(&esp, &[(KERNEL, &kernel), (INITRD, &initrd)])?;
    let started = Instant::now();
    boot(&esp, &log_dir, &serial_log, &host)?;
    check_banner(&serial_log)?;
    check_forbidden(&serial_log)?;
    check_intx(&host)?;
    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        scenario = "uefi-linux-acpi",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

fn boot(esp: &Path, log_dir: &Path, serial_log: &Path, host_log: &Path) -> Result<(), String> {
    let vm = VmConfig {
        name: "uefi-linux-acpi".to_string(),
        cpus: 2,
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
        nvram: Some(log_dir.join("nvram.fd")),
        firmware_tables: Some(FirmwareTables::Acpi),
        vsock: None,
    };
    tracing::info!(target: "ternvale::boot", esp = %esp.display(), serial = %serial_log.display(), "uefi linux acpi boot");
    let uart = Pl011::open(serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let feed_log = serial_log.to_path_buf();
    let host = host_log.to_path_buf();
    let feeder = std::thread::spawn(move || {
        let result = drive(&feed_log, &mut input, &flag, &host, steps());
        flag.store(true, Ordering::Release);
        result
    });
    let disks: Vec<(PathBuf, bool)> = vec![(esp.to_path_buf(), true)];
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        attach_disks_pci(attach, &disks).map(|_| Vec::new())
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
        Ok(ExitReason::Canceled) => Ok(()),
        other => Err(format!(
            "exit={other:?} (expected Canceled after the kernel checks)\nlog={}\n--- serial ---\n{}",
            host_log.display(),
            last_lines(&serial, 30)
        )),
    }
}

fn steps() -> Vec<Step> {
    let mut steps = to_shell(true);
    let command = format!(
        "fs0:\\{KERNEL} initrd=\\{INITRD} {}",
        ternvale_vmm::DEFAULT_CMDLINE
    );
    steps.extend(line(&command, b"\r"));
    for (pattern, secs) in [
        ("EFI stub: Booting Linux Kernel", 120),
        ("Linux version", 60),
        ("ACPI: PCI Root Bridge [PCI0] (domain 0000 [bus 00-0f])", 60),
        (
            "ECAM area [mem 0x3f000000-0x3fffffff] reserved by PNP0C02:00",
            10,
        ),
        ("ECAM at [mem 0x3f000000-0x3fffffff] for [bus 00-0f]", 10),
        (
            "pci_bus 0000:00: root bus resource [mem 0x10000000-0x3effffff window]",
            10,
        ),
        ("pci_bus 0000:00: root bus resource [bus 00-0f]", 10),
        ("pci 0000:00:01.0: [1af4:1042]", 10),
        ("Run /init as init process", 60),
        ("virtio_blk virtio0: [vda]", 30),
        (" vda: vda1", 30),
    ] {
        steps.push(expect(pattern, secs));
    }
    steps
}

fn check_forbidden(serial_log: &Path) -> Result<(), String> {
    let serial = std::fs::read_to_string(serial_log)
        .map_err(|err| format!("read {}: {err}", serial_log.display()))?;
    let bad: Vec<&str> = serial
        .lines()
        .filter(|line| FORBIDDEN.iter().any(|f| line.contains(f)))
        .collect();
    for line in serial
        .lines()
        .filter(|l| l.contains("PCI0") || l.contains("0000:00:01.0"))
    {
        tracing::info!(target: "ternvale::boot", line = %line.trim(), "guest pci line");
    }
    if bad.is_empty() {
        return Ok(());
    }
    tracing::error!(target: "ternvale::boot", lines = %bad.join(" | "), "guest reported acpi or pci routing errors");
    Err(format!("guest reported:\n{}", bad.join("\n")))
}

fn check_intx(host_log: &Path) -> Result<(), String> {
    let log = std::fs::read_to_string(host_log)
        .map_err(|err| format!("read {}: {err}", host_log.display()))?;
    let spi = format!("spi={DISK_SPI} ");
    let toggles = |level: &str| {
        log.lines()
            .filter(|l| l.contains("pci intx line") && l.contains(&spi) && l.contains(level))
            .count()
    };
    let (asserts, deasserts) = (toggles("level=true"), toggles("level=false"));
    tracing::info!(target: "ternvale::boot", spi = DISK_SPI, asserts, deasserts, "disk intx line activity");
    if deasserts >= MIN_DEASSERTS {
        return Ok(());
    }
    Err(format!(
        "SPI {DISK_SPI} asserted {asserts} and deasserted {deasserts} times (want >= {MIN_DEASSERTS} deasserts)\nlog={}",
        host_log.display()
    ))
}
