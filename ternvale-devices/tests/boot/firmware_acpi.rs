//! UEFI scenario with ACPI: EDK2 must install Ternvale's tables from fw_cfg.
//!
//! The config leaves `firmware_tables` unset, which means `acpi` on a
//! firmware boot. In the UEFI shell, the configuration table array must hold
//! an ACPI 2.0 table and no `gFdtTableGuid` entry (EDK2 publishes one or the
//! other; see `uefi_dmem`). The harness then walks EDK2's RSDP →
//! XSDT → each table → the FADT's DSDT with `dmem <addr> <len>`. It compares
//! every table with what `ternvale-acpi` builds for the same vCPU count. Tables
//! without pointers (APIC, GTDT, MCFG, SPCR, DBG2, DSDT) must match byte for
//! byte. The FADT may differ only in its checksum and the FACS/DSDT pointer
//! fields that `AcpiTableDxe` rewrites. The XSDT is EDK2's own, so only its
//! entry signatures are checked. Ends with `reset -s`.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use ternvale_config::{FirmwareTables, VmConfig};
use ternvale_devices::Pl011;
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{init_logging, log_dir, restore_stdin, stdin_pipe, write_result};
use crate::firmware::{check_banner, power_off, to_shell};
use crate::uefi_dmem::{find, u64_at, Shell, ACPI_20_GUID, FDT_GUID};

const CPUS: u32 = 2;
/// Tables the XSDT must list (EDK2 rebuilds the XSDT from what it installed).
const LISTED: [&str; 6] = ["FACP", "APIC", "GTDT", "MCFG", "SPCR", "DBG2"];
/// FADT bytes `AcpiTableDxe` may rewrite: checksum, `FIRMWARE_CTRL`/`DSDT`,
/// `X_FIRMWARE_CTRL`/`X_DSDT`.
const FADT_REWRITTEN: [std::ops::Range<usize>; 3] = [9..10, 36..44, 132..148];

/// One table as EDK2 installed it.
struct Found {
    signature: String,
    gpa: u64,
    bytes: Vec<u8>,
}

pub fn run() -> Result<(), String> {
    let log_dir = log_dir();
    let guard = init_logging("firmware-acpi", &log_dir)?;
    let host = guard.log_path().to_path_buf();
    let nvram = log_dir.join("nvram.fd");
    if nvram.exists() {
        std::fs::remove_file(&nvram).map_err(|err| format!("remove old nvram: {err}"))?;
    }
    let serial_log = log_dir.join("guest-serial.log");
    let vm = VmConfig {
        name: "uefi-acpi".to_string(),
        cpus: CPUS,
        ram_mib: 512,
        kernel: Default::default(),
        initrd: None,
        cmdline: String::new(),
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: serial_log.clone(),
        firmware: Some(crate::common::firmware()),
        nvram: Some(nvram),
        firmware_tables: None,
        vsock: None,
    };
    if vm.effective_firmware_tables() != FirmwareTables::Acpi {
        return Err("a firmware boot without firmware_tables should default to acpi".into());
    }
    let started = Instant::now();
    let uart = Pl011::open(&serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let (feed_log, feed_host) = (serial_log.clone(), host.clone());
    let feeder = std::thread::spawn(move || walk(&feed_log, &mut input, &flag, &feed_host));
    let exit = Machine::run_until(&vm, Box::new(uart), Arc::clone(&cancel));
    let walked = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    let script = walked.as_ref().map(|_| ()).map_err(Clone::clone);
    write_result(&log_dir, &exit, &script, &serial);
    let walk = walked?;
    if !matches!(exit, Ok(ExitReason::SystemOff)) {
        return Err(format!("exit={exit:?}\nlog={}", host.display()));
    }
    check_banner(&serial_log)?;
    let report = compare(&walk)?;
    std::fs::write(log_dir.join("acpi-tables.txt"), &report)
        .map_err(|err| format!("write acpi-tables.txt: {err}"))?;
    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        tables = walk.listed.len() + 2,
        scenario = "firmware-acpi",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

/// What the shell walk found: EDK2's XSDT, the tables it lists, and the
/// FADT's DSDT.
struct Walk {
    xsdt: Found,
    listed: Vec<Found>,
    dsdt: Option<Found>,
}

/// The SDT at `gpa`: header first for the length, then the whole table.
fn table(shell: &mut Shell<'_>, gpa: u64) -> Result<Found, String> {
    let header = shell.dmem(gpa, ternvale_acpi::SDT_HEADER_LEN)?;
    let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if !(ternvale_acpi::SDT_HEADER_LEN..=0x4000).contains(&len) {
        return Err(format!("table at {gpa:#x} claims {len} bytes"));
    }
    let bytes = shell.dmem(gpa, len)?;
    Ok(Found {
        signature: String::from_utf8_lossy(&bytes[..4]).to_string(),
        gpa,
        bytes,
    })
}

/// Drive the shell to the prompt, walk the tables, then power off.
fn walk(
    serial_log: &Path,
    input: &mut std::fs::File,
    cancel: &AtomicBool,
    host: &Path,
) -> Result<Walk, String> {
    let mut shell = Shell {
        serial_log,
        input,
        cancel,
        host,
    };
    shell.run(to_shell(true))?;
    let walked = walk_tables(&mut shell);
    shell.run(power_off())?;
    walked
}

/// EDK2 published an ACPI 2.0 table and no DTB; RSDP → XSDT → tables → DSDT.
fn walk_tables(shell: &mut Shell<'_>) -> Result<Walk, String> {
    let entries = shell.config_tables()?;
    let rsdp = find(&entries, &ACPI_20_GUID);
    let dtb = find(&entries, &FDT_GUID);
    let (Some(rsdp), None) = (rsdp, dtb) else {
        return Err(format!(
            "acpi boot: expected an ACPI 2.0 table and no DTB, got acpi20={rsdp:x?} dtb={dtb:x?}"
        ));
    };
    let rsdp_bytes = shell.dmem(rsdp, ternvale_acpi::RSDP_LEN)?;
    if &rsdp_bytes[..8] != b"RSD PTR " || ternvale_acpi::byte_sum(&rsdp_bytes) != 0 {
        return Err(format!("no valid RSDP at {rsdp:#x}: {rsdp_bytes:02x?}"));
    }
    let xsdt = table(shell, u64_at(&rsdp_bytes, ternvale_acpi::RSDP_XSDT_OFFSET))?;
    let mut listed = Vec::new();
    for entry in xsdt.bytes[ternvale_acpi::SDT_HEADER_LEN..].chunks_exact(8) {
        listed.push(table(shell, u64_at(entry, 0))?);
    }
    let dsdt = match listed.iter().find(|t| t.signature == "FACP") {
        Some(fadt) => Some(table(
            shell,
            u64_at(&fadt.bytes, ternvale_acpi::X_DSDT_OFFSET),
        )?),
        None => None,
    };
    Ok(Walk { xsdt, listed, dsdt })
}

/// One line per table; an error names every mismatch.
fn compare(walk: &Walk) -> Result<String, String> {
    let config = ternvale_vmm::acpi_config(CPUS);
    let built =
        ternvale_acpi::AcpiTables::build(ternvale_vmm::ACPI_BASE, ternvale_vmm::ACPI_SIZE, &config)
            .map_err(|err| format!("build expected tables: {err}"))?;
    let mut report = String::new();
    let mut problems = Vec::new();
    let listed: Vec<&str> = walk.listed.iter().map(|t| t.signature.as_str()).collect();
    let mut sorted = listed.clone();
    sorted.sort_unstable();
    let mut want = LISTED.to_vec();
    want.sort_unstable();
    if sorted != want {
        problems.push(format!("XSDT lists {listed:?}, expected {LISTED:?}"));
    }
    if walk.dsdt.is_none() {
        problems.push("no FADT, so no DSDT".to_string());
    }
    let all = std::iter::once(&walk.xsdt)
        .chain(&walk.listed)
        .chain(&walk.dsdt);
    for table in all {
        let sum_ok = ternvale_acpi::byte_sum(&table.bytes) == 0;
        let verdict = match built
            .tables()
            .iter()
            .find(|t| t.signature == table.signature)
        {
            _ if table.signature == "XSDT" => "edk2's own".to_string(),
            Some(ours) if table.signature == "FACP" => {
                let mask = |bytes: &[u8]| {
                    let mut bytes = bytes.to_vec();
                    for range in FADT_REWRITTEN {
                        bytes[range].fill(0);
                    }
                    bytes
                };
                if table.bytes.len() == ours.bytes.len() && mask(&table.bytes) == mask(&ours.bytes)
                {
                    "matches outside rewritten pointers".to_string()
                } else {
                    problems
                        .push("FACP differs outside the fields AcpiTableDxe rewrites".to_string());
                    "DIFFERS".to_string()
                }
            }
            Some(ours) if ours.bytes == table.bytes => "identical".to_string(),
            Some(_) => {
                problems.push(format!("{} differs from the builder", table.signature));
                "DIFFERS".to_string()
            }
            None => {
                problems.push(format!("unexpected table {}", table.signature));
                "UNEXPECTED".to_string()
            }
        };
        if !sum_ok {
            problems.push(format!("{} checksum is wrong", table.signature));
        }
        report.push_str(&format!(
            "{} gpa={:#x} len={} sum_ok={sum_ok} {verdict}\n",
            table.signature,
            table.gpa,
            table.bytes.len()
        ));
        tracing::info!(target: "ternvale::boot", signature = %table.signature, gpa = %format!("{:#x}", table.gpa), len = table.bytes.len(), sum_ok, verdict = %verdict, "acpi table installed by edk2");
    }
    if problems.is_empty() {
        Ok(report)
    } else {
        Err(format!("{}\n{report}", problems.join("\n")))
    }
}
