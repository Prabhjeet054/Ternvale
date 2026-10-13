//! Which backend `[tpm]` gets, and a real swtpm through the MMIO bus.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_tpm::regs::{
    CTRL_REQ, CTRL_REQ_CMD_READY, CTRL_START, LOC_CTRL, LOC_CTRL_REQUEST_ACCESS, START_INVOKE,
};
use ternvale_tpm::{check_state_file, find_swtpm, StateFile, DATA_BUFFER};

use super::backend_for;
use super::tests::{direct_boot, Guest};

fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-tpm-{}-{test}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear leftovers");
    }
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

/// A firmware boot with its NVRAM in `dir` and `extra` appended.
fn firmware_boot(dir: &Path, extra: &str) -> VmConfig {
    VmConfig::parse(&format!(
        "name = \"t\"\ncpus = 1\nram_mib = 256\nfirmware = \"/fw.fd\"\nnvram = \"{}\"\nserial_log = \"/tmp/s.log\"\n{extra}",
        dir.join("nvram.fd").display()
    ))
    .expect("config")
}

fn swtpm_installed() -> bool {
    if find_swtpm(None).is_ok() {
        return true;
    }
    let required = std::env::var("TERNVALE_REQUIRE_SWTPM").is_ok_and(|v| v == "1");
    assert!(
        !required,
        "TERNVALE_REQUIRE_SWTPM=1 but swtpm is not installed"
    );
    false
}

#[test]
fn guests_without_acpi_and_explicit_stub_get_the_stub() {
    let dir = scratch("stub");
    assert_eq!(backend_for(&direct_boot()).expect("direct").name(), "stub");
    let fdt = firmware_boot(&dir, "firmware_tables = \"fdt\"\n");
    assert_eq!(backend_for(&fdt).expect("fdt").name(), "stub");
    let stub = firmware_boot(&dir, "[tpm]\nbackend = \"stub\"\n");
    assert_eq!(backend_for(&stub).expect("stub").name(), "stub");
    assert!(
        !dir.join("nvram.fd.swtpm").exists(),
        "no swtpm, no state file"
    );
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn auto_falls_back_to_the_stub_and_swtpm_fails_when_swtpm_is_missing() {
    let dir = scratch("missing");
    let fake = dir.join("swtpm");
    std::fs::write(&fake, b"not executable").expect("write");
    let tpm = format!("swtpm = \"{}\"\n", fake.display());
    let auto = firmware_boot(&dir, &format!("[tpm]\n{tpm}"));
    assert_eq!(backend_for(&auto).expect("auto").name(), "stub");
    let required = firmware_boot(&dir, &format!("[tpm]\nbackend = \"swtpm\"\n{tpm}"));
    let error = backend_for(&required).err().expect("swtpm required");
    assert!(error.to_string().contains("brew install swtpm"), "{error}");
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn auto_refuses_a_state_file_swtpm_did_not_write() {
    if !swtpm_installed() {
        return;
    }
    let dir = scratch("foreign");
    std::fs::write(dir.join("nvram.fd.swtpm"), b"something else").expect("write");
    let error = backend_for(&firmware_boot(&dir, ""))
        .err()
        .expect("foreign state");
    assert!(error.to_string().contains("not a swtpm"), "{error}");
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn startup_reaches_swtpm_through_the_mmio_bus_and_state_lands_beside_nvram() {
    if !swtpm_installed() {
        return;
    }
    let dir = scratch("bus");
    let guest = Guest::with(&firmware_boot(&dir, ""));
    guest.write(LOC_CTRL, 4, u64::from(LOC_CTRL_REQUEST_ACCESS));
    guest.write(CTRL_REQ, 4, u64::from(CTRL_REQ_CMD_READY));
    // TPM2_Startup(TPM_SU_CLEAR).
    let command: [u8; 12] = [0x80, 0x01, 0, 0, 0, 12, 0, 0, 0x01, 0x44, 0, 0];
    for (i, chunk) in command.chunks(4).enumerate() {
        let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        guest.write(DATA_BUFFER + 4 * i as u64, 4, u64::from(word));
    }
    guest.write(CTRL_START, 4, u64::from(START_INVOKE));
    let deadline = Instant::now() + Duration::from_secs(30);
    while guest.read(CTRL_START, 4) != 0 {
        assert!(Instant::now() < deadline, "START never cleared");
        std::thread::sleep(Duration::from_millis(1));
    }
    let header: Vec<u8> = (0..10)
        .map(|i| guest.read(DATA_BUFFER + i, 1) as u8)
        .collect();
    assert_eq!(
        header,
        [0x80, 0x01, 0, 0, 0, 10, 0, 0, 0, 0],
        "TPM_RC_SUCCESS"
    );
    drop(guest);
    let state = dir.join("nvram.fd.swtpm");
    assert!(matches!(
        check_state_file(&state),
        Ok(StateFile::Existing { .. })
    ));
    std::fs::remove_dir_all(&dir).expect("cleanup");
}
