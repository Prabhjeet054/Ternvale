//! Debug-EDK2 scenario: EDK2's own log shows it installed the ACPI tables
//! from fw_cfg (`firmware_tables = "acpi"`) instead of falling back to DT.
//!
//! The firmware is `TERNVALE_FIRMWARE`, else `test-assets/firmware-debug/
//! QEMU_EFI.fd` (`./scripts/fetch-debug-firmware.sh`, a verbose `DEBUG_GCC`
//! ArmVirtQemu). It boots until BDS starts (`[Bds]`) and is then cancelled.
//! That build has no built-in UEFI shell to power off from.
//!
//! Required lines: `Found FwCfg @ 0x9020008/0x9020000` (and no `Found FwCfg
//! DMA`); `OnRootBridgesConnected: … installing ACPI tables`;
//! `InstallQemuFwCfgTables: installed 7 tables` (FACP, APIC, GTDT, MCFG,
//! SPCR, DBG2, DSDT). Forbidden: FdtClientDxe's DT fallback `exposing DTB …
//! to OS` and AcpiPlatformDxe's failure line `InstallAcpiTables: <status>`.
//!
//! Messages are from `OvmfPkg/AcpiPlatformDxe/EntryPoint.c`,
//! `OvmfPkg/Library/AcpiPlatformLib/QemuFwCfgAcpi.c`,
//! `OvmfPkg/Library/QemuFwCfgLib/QemuFwCfgMmioPei.c`, and
//! `EmbeddedPkg/Drivers/FdtClientDxe/FdtClientDxe.c`.
//!
//! `firmware_tables = "fdt"` is not checked here: EDK2 master since
//! 2026-08-28 ("ArmVirtPkg: Map QEMU fw_cfg MMIO region in PEI") asserts in
//! `ArmVirtGetMemoryMap` without a `qemu,fw-cfg-mmio` node, which `fdt`
//! mode omits. Released EDK2 (up to edk2-stable202608) boots it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use ternvale_config::VmConfig;
use ternvale_devices::{last_lines, Pl011};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{assets_root, drive, init_logging, log_dir, restore_stdin, stdin_pipe};
use crate::firmware::{check_banner, expect};

const PRESENT: [&str; 3] = [
    "Found FwCfg @ 0x9020008/0x9020000",
    "OnRootBridgesConnected: root bridges have been connected, installing ACPI tables",
    // Every builder table but the RSDP and XSDT.
    "InstallQemuFwCfgTables: installed 7 tables",
];
const ABSENT: [&str; 3] = ["exposing DTB", "Found FwCfg DMA", "InstallAcpiTables: "];

pub fn run() -> Result<(), String> {
    let log_dir = log_dir();
    let guard = init_logging("firmware-debug", &log_dir)?;
    let host = guard.log_path().to_path_buf();
    let firmware = match std::env::var_os("TERNVALE_FIRMWARE") {
        Some(path) => PathBuf::from(path),
        None => assets_root().join("firmware-debug/QEMU_EFI.fd"),
    };
    if !firmware.is_file() {
        return Err(format!(
            "missing {} (run ./scripts/fetch-debug-firmware.sh)",
            firmware.display()
        ));
    }
    let started = Instant::now();
    let serial_log = log_dir.join("guest-serial.log");
    boot(&firmware, &serial_log, &log_dir, &host)?;
    check_banner(&serial_log)?;
    check_log(&serial_log)?;
    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        scenario = "firmware-debug",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

fn boot(firmware: &Path, serial_log: &Path, log_dir: &Path, host_log: &Path) -> Result<(), String> {
    let vm = VmConfig {
        name: "uefi-debug".to_string(),
        cpus: 1,
        ram_mib: 512,
        kernel: Default::default(),
        initrd: None,
        cmdline: String::new(),
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: serial_log.to_path_buf(),
        firmware: Some(firmware.to_path_buf()),
        nvram: Some(log_dir.join("nvram.fd")),
        firmware_tables: None,
        vsock: None,
    };
    tracing::info!(
        target: "ternvale::boot",
        firmware = %firmware.display(),
        firmware_tables = %vm.effective_firmware_tables(),
        serial = %serial_log.display(),
        "debug firmware boot"
    );
    let uart = Pl011::open(serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let feed_log = serial_log.to_path_buf();
    let host = host_log.to_path_buf();
    let steps = vec![expect("UEFI firmware", 60), expect("[Bds]", 120)];
    let feeder = std::thread::spawn(move || {
        let result = drive(&feed_log, &mut input, &flag, &host, steps);
        flag.store(true, Ordering::Release);
        result
    });
    let exit = Machine::run_until(&vm, Box::new(uart), Arc::clone(&cancel));
    cancel.store(true, Ordering::Release);
    let script = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let serial = std::fs::read_to_string(serial_log).unwrap_or_default();
    crate::common::write_result(log_dir, &exit, &script, &serial);
    script?;
    match exit {
        Ok(ExitReason::Canceled) => Ok(()),
        other => Err(format!(
            "exit={other:?} (expected Canceled at BDS)\nlog={}\n--- serial ---\n{}",
            host_log.display(),
            last_lines(&serial, 30)
        )),
    }
}

/// Every `PRESENT` line is in the serial log and no `ABSENT` text is.
fn check_log(serial_log: &Path) -> Result<(), String> {
    let serial = std::fs::read_to_string(serial_log)
        .map_err(|err| format!("read {}: {err}", serial_log.display()))?;
    let text = crate::uefi_dmem::strip(&serial);
    let mut problems = Vec::new();
    for want in PRESENT {
        match text.lines().find(|line| line.contains(want)) {
            Some(line) => {
                tracing::info!(target: "ternvale::boot", line = %line.trim(), "edk2 debug log line found")
            }
            None => problems.push(format!("missing {want:?}")),
        }
    }
    for unwanted in ABSENT {
        if let Some(line) = text.lines().find(|line| line.contains(unwanted)) {
            problems.push(format!("unexpected {unwanted:?}: {}", line.trim()));
        }
    }
    if problems.is_empty() {
        tracing::info!(target: "ternvale::boot", "edk2 installed the acpi tables from fw_cfg; no dt fallback");
        Ok(())
    } else {
        tracing::error!(target: "ternvale::boot", problems = %problems.join("; "), "edk2 debug log check failed");
        Err(problems.join("\n"))
    }
}
