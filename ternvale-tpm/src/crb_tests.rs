use super::*;
use crate::backend::{
    error_response, StubBackend, HEADER_LEN, RC_COMMAND_CODE, RC_COMMAND_SIZE, RC_FAILURE,
    ST_NO_SESSIONS, ST_RSP_COMMAND,
};
use crate::RC_BAD_TAG;

pub(super) const BASE: u64 = 0x0914_0000;
/// `TPM2_Startup(TPM_SU_CLEAR)`.
pub(super) const STARTUP: [u8; 12] = [0x80, 0x01, 0, 0, 0, 12, 0, 0, 0x01, 0x44, 0, 0];

pub(super) fn crb() -> TpmCrb {
    TpmCrb::new(BASE, Box::new(StubBackend::new())).expect("crb")
}

pub(super) fn page(locality: u8, reg: u64) -> u64 {
    u64::from(locality) * LOCALITY_STRIDE + reg
}

/// Write `bytes` into the data buffer through `locality`'s page, 4 bytes at a
/// time with a byte-wide tail, as a driver's `memcpy_toio` would.
pub(super) fn put(crb: &mut TpmCrb, locality: u8, bytes: &[u8]) {
    let mut chunks = bytes.chunks_exact(4);
    let mut at = page(locality, DATA_BUFFER);
    for chunk in &mut chunks {
        let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        crb.write(at, 4, u64::from(word));
        at += 4;
    }
    for byte in chunks.remainder() {
        crb.write(at, 1, u64::from(*byte));
        at += 1;
    }
}

pub(super) fn get(crb: &mut TpmCrb, locality: u8, len: usize) -> Vec<u8> {
    (0..len as u64)
        .map(|i| crb.read(page(locality, DATA_BUFFER) + i, 1) as u8)
        .collect()
}

/// The Linux `tpm_crb` sequence for one command at `locality`.
fn run(crb: &mut TpmCrb, locality: u8, command: &[u8]) -> Vec<u8> {
    crb.write(
        page(locality, LOC_CTRL),
        4,
        u64::from(LOC_CTRL_REQUEST_ACCESS),
    );
    crb.write(page(locality, CTRL_REQ), 4, u64::from(CTRL_REQ_CMD_READY));
    put(crb, locality, command);
    crb.write(page(locality, CTRL_START), 4, u64::from(START_INVOKE));
    assert_eq!(crb.read(page(locality, CTRL_START), 4), 0, "start clears");
    let header = get(crb, locality, HEADER_LEN);
    let size = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
    let response = get(crb, locality, size.min(DATA_BUFFER_SIZE));
    crb.write(page(locality, CTRL_REQ), 4, u64::from(CTRL_REQ_GO_IDLE));
    crb.write(page(locality, LOC_CTRL), 4, u64::from(LOC_CTRL_RELINQUISH));
    response
}

#[test]
fn interface_id_says_crb_with_five_localities() {
    let mut crb = crb();
    let id = crb.read(INTF_ID_LO, 8);
    assert_eq!(id & 0xf, 1, "InterfaceType: CRB active");
    assert_eq!(id >> 4 & 0xf, 1, "InterfaceVersion: CRB");
    assert_eq!(id >> 8 & 1, 1, "CapLocality");
    assert_eq!(id >> 11 & 3, 3, "64-byte transfers");
    assert_eq!(id >> 13 & 1, 0, "no FIFO");
    assert_eq!(id >> 14 & 1, 1, "CapCRB");
    assert_eq!(id >> 17 & 3, 1, "InterfaceSelector: CRB");
    assert_eq!(id >> 32 & 0xffff, 0x1014, "VID");
    assert_eq!(crb.read(INTF_ID_LO, 4), id & 0xffff_ffff);
    assert_eq!(crb.read(INTF_ID_HI, 4), id >> 32);
    assert_eq!(crb.read(page(3, INTF_ID_LO), 8), id, "same in every page");
}

#[test]
fn control_area_points_at_each_pages_buffer() {
    let mut crb = crb();
    for locality in 0..LOCALITIES {
        let buffer = BASE + page(locality, DATA_BUFFER);
        assert_eq!(crb.read(page(locality, CMD_SIZE), 4), 0xf80);
        assert_eq!(crb.read(page(locality, RSP_SIZE), 4), 0xf80);
        assert_eq!(crb.read(page(locality, CMD_LADDR), 4), buffer & 0xffff_ffff);
        assert_eq!(crb.read(page(locality, CMD_HADDR), 4), buffer >> 32);
        assert_eq!(crb.read(page(locality, RSP_ADDR_LO), 8), buffer);
    }
    assert_eq!(CONTROL_AREA_OFFSET, 0x40);
    assert_eq!(CRB_WINDOW_SIZE, 0x5000);
}

#[test]
fn locality_registers_follow_requests() {
    let mut crb = crb();
    assert_eq!(
        crb.read(LOC_STATE, 4),
        0x81,
        "valid, established, unassigned"
    );
    crb.write(page(2, LOC_CTRL), 4, u64::from(LOC_CTRL_REQUEST_ACCESS));
    assert_eq!(crb.read(LOC_STATE, 4), 0x81 | 0b10 | (2 << 2));
    assert_eq!(crb.read(page(4, LOC_STATE), 1), 0x8b, "byte read, any page");
    assert_eq!(crb.read(page(2, LOC_STS), 4), 1, "granted");
    assert_eq!(crb.read(page(0, LOC_STS), 4), 0);
    crb.write(page(2, LOC_CTRL), 1, u64::from(LOC_CTRL_RELINQUISH));
    assert_eq!(crb.read(LOC_STATE, 4), 0x81);
}

#[test]
fn cmd_ready_and_go_idle_drive_tpm_idle() {
    let mut crb = crb();
    assert_eq!(crb.read(CTRL_STS, 4), u64::from(CTRL_STS_IDLE));
    crb.write(CTRL_REQ, 4, u64::from(CTRL_REQ_CMD_READY));
    assert_eq!(crb.read(CTRL_STS, 4), 0);
    assert_eq!(crb.read(CTRL_REQ, 4), 0, "requests complete at once");
    crb.write(
        CTRL_REQ,
        4,
        u64::from(CTRL_REQ_CMD_READY | CTRL_REQ_GO_IDLE),
    );
    assert_eq!(crb.read(CTRL_STS, 4), 0, "both bits together ignored");
    crb.write(CTRL_REQ, 4, u64::from(CTRL_REQ_GO_IDLE));
    assert_eq!(crb.read(CTRL_STS, 4), u64::from(CTRL_STS_IDLE));
}

#[test]
fn a_command_gets_the_stub_response_in_the_buffer() {
    let mut crb = crb();
    let response = run(&mut crb, 0, &STARTUP);
    assert_eq!(response, error_response(ST_NO_SESSIONS, RC_COMMAND_CODE));
    assert_eq!(crb.commands(), 1);
    let response = run(&mut crb, 3, &STARTUP);
    assert_eq!(response, error_response(ST_NO_SESSIONS, RC_COMMAND_CODE));
    assert_eq!(crb.commands(), 2);
}

#[test]
fn start_from_a_locality_that_is_not_active_does_nothing() {
    let mut crb = crb();
    crb.write(page(0, LOC_CTRL), 4, u64::from(LOC_CTRL_REQUEST_ACCESS));
    put(&mut crb, 1, &STARTUP);
    crb.write(page(1, CTRL_START), 4, u64::from(START_INVOKE));
    assert_eq!(crb.commands(), 0);
    assert_eq!(get(&mut crb, 0, STARTUP.len()), STARTUP, "buffer untouched");
    crb.write(page(0, LOC_CTRL), 4, u64::from(LOC_CTRL_RELINQUISH));
    crb.write(page(0, CTRL_START), 4, u64::from(START_INVOKE));
    assert_eq!(crb.commands(), 0, "no locality assigned");
}

#[test]
fn bad_command_sizes_and_tags_get_error_responses() {
    let mut crb = crb();
    let mut huge = STARTUP;
    huge[2..6].copy_from_slice(&0x1000u32.to_be_bytes());
    assert_eq!(
        run(&mut crb, 0, &huge),
        error_response(ST_NO_SESSIONS, RC_COMMAND_SIZE)
    );
    let mut short = STARTUP;
    short[2..6].copy_from_slice(&4u32.to_be_bytes());
    assert_eq!(
        run(&mut crb, 0, &short),
        error_response(ST_NO_SESSIONS, RC_COMMAND_SIZE)
    );
    let mut tpm12 = STARTUP;
    tpm12[..2].copy_from_slice(&0x00c1u16.to_be_bytes());
    assert_eq!(
        run(&mut crb, 0, &tpm12),
        error_response(ST_RSP_COMMAND, RC_BAD_TAG)
    );
}

struct Garbage(Vec<u8>);

impl TpmBackend for Garbage {
    fn name(&self) -> &str {
        "garbage"
    }

    fn execute(&mut self, _locality: u8, _command: &[u8]) -> Vec<u8> {
        self.0.clone()
    }
}

#[test]
fn malformed_backend_responses_become_tpm_rc_failure() {
    let failure = error_response(ST_NO_SESSIONS, RC_FAILURE);
    for bad in [vec![0x80, 0x01], vec![0; DATA_BUFFER_SIZE + 1], {
        let mut lies = error_response(ST_NO_SESSIONS, 0);
        lies[5] = 11;
        lies
    }] {
        let mut crb = TpmCrb::new(BASE, Box::new(Garbage(bad))).expect("crb");
        assert_eq!(run(&mut crb, 0, &STARTUP), failure);
    }
}

#[test]
fn bad_accesses_read_zero_and_write_nothing() {
    let mut crb = crb();
    assert_eq!(crb.read(LOC_STATE + 1, 2), 0, "misaligned");
    assert_eq!(crb.read(LOC_STATE, 3), 0, "size 3");
    assert_eq!(crb.read(CRB_WINDOW_SIZE, 4), 0, "past the window");
    assert_eq!(crb.read(u64::MAX - 3, 4), 0, "overflow");
    crb.write(LOC_CTRL + 2, 4, u64::from(LOC_CTRL_REQUEST_ACCESS));
    crb.write(CRB_WINDOW_SIZE - 2, 4, 1);
    assert_eq!(crb.read(LOC_STATE, 4), 0x81, "no locality granted");
    crb.write(CMD_LADDR, 4, 0x1234);
    assert_eq!(crb.read(CMD_LADDR, 4), (BASE + DATA_BUFFER) & 0xffff_ffff);
}

#[test]
fn interrupt_enable_is_stored_and_status_stays_zero() {
    let mut crb = crb();
    crb.write(INT_ENABLE, 4, 0x8000_0001);
    assert_eq!(crb.read(INT_ENABLE, 4), 0x8000_0001);
    crb.write(INT_ENABLE + 3, 1, 0);
    assert_eq!(crb.read(INT_ENABLE, 4), 1, "byte write merges");
    crb.write(INT_STS, 4, 1);
    assert_eq!(crb.read(INT_STS, 4), 0);
    crb.write(CTRL_CANCEL, 4, 1);
    assert_eq!(crb.read(CTRL_CANCEL, 4), 1);
}

#[test]
fn new_rejects_unaligned_and_overflowing_bases() {
    let stub = || Box::new(StubBackend::new());
    assert_eq!(
        TpmCrb::new(BASE + 4, stub()).unwrap_err(),
        TpmError::Misaligned { base: BASE + 4 }
    );
    assert_eq!(
        TpmCrb::new(u64::MAX & !0xfff, stub()).unwrap_err(),
        TpmError::Overflow {
            base: u64::MAX & !0xfff
        }
    );
}

#[test]
fn every_command_and_response_byte_stream_is_logged_at_trace() {
    let ((), text) = crate::test_log::capture(|| {
        let mut crb = crb();
        run(&mut crb, 0, &STARTUP);
    });
    assert!(
        text.contains("tpm command") && text.contains("bytes=80010000000c000001440000"),
        "{text}"
    );
    assert!(
        text.contains("tpm response") && text.contains("bytes=80010000000a00000143"),
        "{text}"
    );
    assert!(text.contains("code=0x144"), "{text}");
    assert!(
        text.contains("tpm crb access locality=0 reg=0x4c size=4 value=0x1 direction=\"write\""),
        "START write missing in\n{text}"
    );
    assert!(
        text.contains("tpm crb access locality=0 reg=0x4c size=4 value=0x0 direction=\"read\""),
        "START poll missing in\n{text}"
    );
}
