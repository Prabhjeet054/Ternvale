//! The CRB over a real swtpm. Skipped when swtpm is not installed, unless
//! `TERNVALE_REQUIRE_SWTPM=1` makes that a failure.

use std::path::Path;
use std::time::{Duration, Instant};

use super::tests::scratch;
use super::*;
use crate::regs::*;
use crate::TpmCrb;

const STARTUP: [u8; 12] = [0x80, 0x01, 0, 0, 0, 12, 0, 0, 0x01, 0x44, 0, 0];
/// `TPM2_GetRandom(8)`.
const GET_RANDOM: [u8; 12] = [0x80, 0x01, 0, 0, 0, 12, 0, 0, 0x01, 0x7b, 0, 8];
/// `TPM2_GetCapability(TPM_CAP_TPM_PROPERTIES, TPM_PT_MAX_COMMAND_SIZE, 2)`.
const MAX_SIZES: [u8; 22] = [
    0x80, 0x01, 0, 0, 0, 22, 0, 0, 0x01, 0x7a, 0, 0, 0, 6, 0, 0, 0x01, 0x1e, 0, 0, 0, 2,
];

fn swtpm() -> Option<SwtpmInfo> {
    match find_swtpm(None) {
        Ok(info) => Some(info),
        Err(error) => {
            let required = std::env::var("TERNVALE_REQUIRE_SWTPM").is_ok_and(|v| v == "1");
            assert!(!required, "TERNVALE_REQUIRE_SWTPM=1 but {error}");
            None
        }
    }
}

fn crb_on(info: &SwtpmInfo, state: &Path) -> TpmCrb {
    let backend = SwtpmBackend::start(Some(&info.path), state).expect("start swtpm");
    let mut crb = TpmCrb::new(0x0914_0000, Box::new(backend)).expect("crb");
    crb.write(LOC_CTRL, 4, u64::from(LOC_CTRL_REQUEST_ACCESS));
    crb
}

/// EDK2's CRB sequence at locality 0: idle to ready, fill, `START`, poll
/// `START` until it clears, read the response.
fn run(crb: &mut TpmCrb, command: &[u8]) -> Vec<u8> {
    crb.write(CTRL_REQ, 4, u64::from(CTRL_REQ_CMD_READY));
    for (i, byte) in command.iter().enumerate() {
        crb.write(DATA_BUFFER + i as u64, 1, u64::from(*byte));
    }
    crb.write(CTRL_START, 4, u64::from(START_INVOKE));
    let deadline = Instant::now() + Duration::from_secs(60);
    while crb.read(CTRL_START, 4) != 0 {
        assert!(Instant::now() < deadline, "START never cleared");
        std::thread::sleep(Duration::from_millis(1));
    }
    let size = (2..6).fold(0usize, |n, i| {
        n << 8 | crb.read(DATA_BUFFER + i, 1) as usize
    });
    (0..size as u64)
        .map(|i| crb.read(DATA_BUFFER + i, 1) as u8)
        .collect()
}

fn rc(response: &[u8]) -> u32 {
    u32::from_be_bytes([response[6], response[7], response[8], response[9]])
}

/// `TPM2_CreatePrimary(TPM_RH_ENDORSEMENT)` with an ECC P-256 restricted
/// decryption template; the returned public area is the TPM's identity.
fn endorsement_key(crb: &mut TpmCrb) -> Vec<u8> {
    let template = [
        "0023", "000b", "000300b2", "0000", "0006", "0080", "0043", "0010", "0003", "0010", "0000",
        "0000",
    ]
    .concat();
    let template = hex(&template);
    // tag, size (patched below), code, primaryHandle, a 9-byte password
    // session, empty inSensitive, inPublic, empty outsideInfo and PCRs.
    let mut command = [
        hex("80020000000000000131"),
        hex("4000000b"),
        hex("00000009"),
        hex("400000090000010000"),
        hex("000400000000"),
        (template.len() as u16).to_be_bytes().to_vec(),
        template,
        hex("000000000000"),
    ]
    .concat();
    let len = command.len() as u32;
    command[2..6].copy_from_slice(&len.to_be_bytes());
    let response = run(crb, &command);
    assert_eq!(rc(&response), 0, "CreatePrimary: {response:02x?}");
    let size = usize::from(u16::from_be_bytes([response[18], response[19]]));
    response[20..20 + size].to_vec()
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

#[test]
fn guest_commands_reach_swtpm_through_the_crb() {
    let Some(info) = swtpm() else { return };
    let dir = scratch("real-crb");
    let mut crb = crb_on(&info, &dir.join("nvram.fd.swtpm"));
    assert_eq!(rc(&run(&mut crb, &STARTUP)), 0);
    let random = run(&mut crb, &GET_RANDOM);
    assert_eq!(
        (rc(&random), random.len(), &random[10..12]),
        (0, 20, &[0u8, 8][..])
    );
    let sizes = run(&mut crb, &MAX_SIZES);
    assert_eq!(rc(&sizes), 0);
    let value =
        |at: usize| u32::from_be_bytes([sizes[at], sizes[at + 1], sizes[at + 2], sizes[at + 3]]);
    assert_eq!(
        (value(23), value(31)),
        (DATA_BUFFER_SIZE as u32, DATA_BUFFER_SIZE as u32)
    );
    drop(crb);
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn tpm_identity_survives_a_restart_on_the_same_state_file() {
    let Some(info) = swtpm() else { return };
    let dir = scratch("real-identity");
    let state = dir.join("nvram.fd.swtpm");
    let ek = |path: &Path| {
        let mut crb = crb_on(&info, path);
        assert_eq!(rc(&run(&mut crb, &STARTUP)), 0);
        endorsement_key(&mut crb)
    };
    let first = ek(&state);
    let header = std::fs::read(&state).expect("state written");
    assert_eq!(
        check_state_file(&state),
        Ok(StateFile::Existing {
            bytes: header.len() as u64
        })
    );
    assert_eq!(ek(&state), first, "same state file, same identity");
    assert_ne!(
        ek(&dir.join("other.swtpm")),
        first,
        "new state file, new identity"
    );
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn a_killed_swtpm_fails_later_commands_and_the_history_says_why() {
    let Some(info) = swtpm() else { return };
    let dir = scratch("real-killed");
    let backend =
        SwtpmBackend::start(Some(&info.path), &dir.join("nvram.fd.swtpm")).expect("start");
    let pid = backend.pid().expect("ternvale started swtpm");
    let mut crb = TpmCrb::new(0x0914_0000, Box::new(backend)).expect("crb");
    crb.write(LOC_CTRL, 4, u64::from(LOC_CTRL_REQUEST_ACCESS));
    let history = crb.history();
    assert_eq!(rc(&run(&mut crb, &STARTUP)), 0);
    let killed = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .expect("run kill");
    assert!(killed.success(), "kill -KILL {pid}: {killed}");
    assert_eq!(rc(&run(&mut crb, &GET_RANDOM)), crate::backend::RC_FAILURE);
    assert_eq!(rc(&run(&mut crb, &STARTUP)), crate::backend::RC_FAILURE);
    let failure = history.failure().expect("failure recorded");
    assert_eq!(failure.seq, 2, "{failure:?}");
    assert!(
        failure.reason.starts_with(&format!(
            "swtpm (pid {pid}) exited with signal: 9 (SIGKILL): swtpm data channel: "
        )) && failure.reason.ends_with(", during GetRandom (0x17b)"),
        "{failure:?}"
    );
    let text = history.render();
    assert!(
        text.contains("seq=3 ") && text.contains("rc=0x101"),
        "{text}"
    );
    drop(crb);
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn a_state_file_in_use_by_another_swtpm_is_refused() {
    let Some(info) = swtpm() else { return };
    let dir = scratch("real-lock");
    let state = dir.join("nvram.fd.swtpm");
    let first = SwtpmBackend::start(Some(&info.path), &state).expect("first");
    let second = SwtpmBackend::start(Some(&info.path), &state).expect_err("locked");
    assert!(second.to_string().contains("Could not lock"), "{second}");
    drop(first);
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn foreign_or_newer_state_files_are_left_untouched() {
    let Some(info) = swtpm() else { return };
    let dir = scratch("real-foreign");
    let state = dir.join("nvram.fd.swtpm");
    let foreign = b"not a tpm state file, swtpm would format it".to_vec();
    std::fs::write(&state, &foreign).expect("write");
    let error = SwtpmBackend::start(Some(&info.path), &state).expect_err("foreign");
    assert!(matches!(error, SwtpmError::State { .. }), "{error}");
    assert_eq!(std::fs::read(&state).expect("read"), foreign);

    let mut newer = 0x7377_7470_6d6c_696eu64.to_le_bytes().to_vec();
    newer.extend_from_slice(&[2, 0, 0xc0, 0]);
    newer.resize(8384, 0);
    std::fs::write(&state, &newer).expect("write");
    let error = SwtpmBackend::start(Some(&info.path), &state).expect_err("newer");
    assert!(
        matches!(error, SwtpmError::StateVersion { found: 2, .. }),
        "{error}"
    );
    assert_eq!(std::fs::read(&state).expect("read"), newer);
    std::fs::remove_dir_all(&dir).expect("cleanup");
}
