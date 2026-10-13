//! UEFI scenario with a real TPM: EDK2 boots twice with `[tpm] backend =
//! "swtpm"` on one NVRAM file, so swtpm keeps its state in `nvram.fd.swtpm`.
//!
//! Each boot goes to the UEFI shell and powers off with `reset -s`. EDK2's
//! `TPM2_Startup` must succeed (`rc=0x0` from swtpm, in the host log). The
//! first boot creates the state file (`new_tpm=true`), the second reuses it
//! (`new_tpm=false`). After each boot the harness starts its own swtpm on the
//! state file and derives the endorsement primary key: the TPM's identity.
//! Both must be equal, so the identity survived a restart of the VM.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use ternvale_config::{TpmBackendKind, TpmSection, VmConfig};
use ternvale_devices::Pl011;
use ternvale_tpm::{check_state_file, StateFile, SwtpmBackend, TpmBackend};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{init_logging, log_dir, restore_stdin, stdin_pipe, write_result};
use crate::firmware::{check_banner, power_off, to_shell};
use crate::uefi_dmem::Shell;

/// `TPM2_Startup(TPM_SU_CLEAR)`.
const STARTUP: [u8; 12] = [0x80, 0x01, 0, 0, 0, 12, 0, 0, 0x01, 0x44, 0, 0];

pub fn run() -> Result<(), String> {
    let log_dir = log_dir();
    let guard = init_logging("firmware-tpm", &log_dir)?;
    let host = guard.log_path().to_path_buf();
    let nvram = log_dir.join("nvram.fd");
    let state = log_dir.join("nvram.fd.swtpm");
    for old in [&nvram, &state] {
        if old.exists() {
            std::fs::remove_file(old).map_err(|err| format!("remove {}: {err}", old.display()))?;
        }
    }
    let started = Instant::now();
    let mut identities = Vec::new();
    for boot in 1..=2 {
        let serial = log_dir.join(format!("guest-serial-{boot}.log"));
        boot_once(&log_dir, &serial, &nvram, &host)?;
        check_banner(&serial)?;
        let bytes = match check_state_file(&state) {
            Ok(StateFile::Existing { bytes }) => bytes,
            other => {
                return Err(format!(
                    "after boot {boot}: state file {}: {other:?}",
                    state.display()
                ))
            }
        };
        let identity = endorsement_key(&state)?;
        let point = &identity[identity.len().saturating_sub(16)..];
        tracing::info!(target: "ternvale::boot", boot, state_bytes = bytes, ek_point_tail = %hex(point), "tpm identity after boot");
        identities.push(identity);
    }
    check_host_log(&host)?;
    if identities[0] != identities[1] {
        return Err(format!(
            "tpm identity changed across a restart: {} vs {}",
            hex(&identities[0]),
            hex(&identities[1])
        ));
    }
    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        scenario = "firmware-tpm",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

fn boot_once(log_dir: &Path, serial_log: &Path, nvram: &Path, host: &Path) -> Result<(), String> {
    let vm = VmConfig {
        name: "uefi-tpm".to_string(),
        cpus: 1,
        ram_mib: 512,
        kernel: Default::default(),
        initrd: None,
        cmdline: String::new(),
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: serial_log.to_path_buf(),
        firmware: Some(crate::common::firmware()),
        nvram: Some(nvram.to_path_buf()),
        firmware_tables: None,
        vsock: None,
        tpm: Some(TpmSection {
            backend: TpmBackendKind::Swtpm,
            swtpm: None,
        }),
    };
    let uart = Pl011::open(serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let (feed_log, feed_host) = (serial_log.to_path_buf(), host.to_path_buf());
    let feeder = std::thread::spawn(move || {
        let mut shell = Shell {
            serial_log: &feed_log,
            input: &mut input,
            cancel: &flag,
            host: &feed_host,
        };
        shell.run(to_shell(true))?;
        shell.run(power_off())
    });
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
        other => Err(format!("exit={other:?}\nlog={}", host.display())),
    }
}

/// swtpm starts in order: boot 1 (a new TPM), the harness's identity read,
/// boot 2 and the second read (all on the existing file); and two successful
/// `TPM2_Startup`s from EDK2 through swtpm.
fn check_host_log(host: &Path) -> Result<(), String> {
    let log =
        std::fs::read_to_string(host).map_err(|err| format!("read {}: {err}", host.display()))?;
    let ready: Vec<bool> = log
        .lines()
        .filter(|line| line.contains("swtpm ready"))
        .map(|line| line.contains("new_tpm=true"))
        .collect();
    let startups = log
        .lines()
        .filter(|line| first_command(line) == Some(("0x144", "0x0", "swtpm")))
        .count();
    if ready != [true, false, false, false] || startups != 2 {
        return Err(format!(
            "expected swtpm ready new_tpm=[true, false, false, false] and 2 successful TPM2_Startup, got {ready:?} and {startups}\nlog={}",
            host.display()
        ));
    }
    tracing::info!(target: "ternvale::boot", "edk2 started the swtpm-backed tpm on both boots; the second reused the state file");
    Ok(())
}

/// The public area of `TPM2_CreatePrimary(TPM_RH_ENDORSEMENT)` with an ECC
/// P-256 restricted decryption template: a function of the TPM's endorsement
/// seed, so equal only for the same TPM.
fn endorsement_key(state: &Path) -> Result<Vec<u8>, String> {
    // type ECC, nameAlg SHA-256, attributes, empty policy, AES-128-CFB,
    // scheme null, curve P-256, kdf null, empty unique x and y.
    let template = unhex(
        &[
            "0023", "000b", "000300b2", "0000", "0006", "0080", "0043", "0010", "0003", "0010",
            "0000", "0000",
        ]
        .concat(),
    );
    create_primary(state, template)
}

/// Start swtpm on `state` from the host, run `TPM2_Startup`, and return the
/// `outPublic` (TPMT_PUBLIC, without its size) of an endorsement-hierarchy
/// `TPM2_CreatePrimary` with `template`.
pub fn create_primary(state: &Path, template: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut tpm = SwtpmBackend::start(None, state).map_err(|err| format!("host swtpm: {err}"))?;
    let startup = tpm.execute(0, &STARTUP);
    if startup.get(6..10) != Some(&[0, 0, 0, 0][..]) {
        return Err(format!("host TPM2_Startup: {startup:02x?}"));
    }
    let mut command = [
        // tag, size (patched below), TPM_CC_CreatePrimary.
        unhex("80020000000000000131"),
        // TPM_RH_ENDORSEMENT, a 9-byte empty password session, empty inSensitive.
        unhex(
            &[
                "4000000b", "00000009", "40000009", "0000", "01", "0000", "0004", "00000000",
            ]
            .concat(),
        ),
        (template.len() as u16).to_be_bytes().to_vec(),
        template,
        unhex("000000000000"),
    ]
    .concat();
    let len = command.len() as u32;
    command[2..6].copy_from_slice(&len.to_be_bytes());
    let response = tpm.execute(0, &command);
    if response.get(6..10) != Some(&[0, 0, 0, 0][..]) || response.len() < 20 {
        return Err(format!(
            "host TPM2_CreatePrimary: {:02x?}",
            &response[..response.len().min(10)]
        ));
    }
    let size = usize::from(u16::from_be_bytes([response[18], response[19]]));
    response
        .get(20..20 + size)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| "CreatePrimary outPublic overruns the response".to_string())
}

/// `(code, rc, backend)` of a "tpm first command from the guest" log line.
pub fn first_command(line: &str) -> Option<(&str, &str, &str)> {
    if !line.contains("tpm first command from the guest") {
        return None;
    }
    let field = |name: &str| {
        let key = format!(" {name}=");
        let at = line.find(&key)? + key.len();
        line[at..]
            .split_whitespace()
            .next()
            .map(|v| v.trim_matches('"'))
    };
    Some((field("code")?, field("rc")?, field("backend")?))
}

pub fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .filter_map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok())
        .collect()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
