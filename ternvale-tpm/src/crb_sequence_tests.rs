//! Register-level sequences: locality claim and release, a command handed to
//! the backend on `START`, the response read back, and EDK2's CRB flow.

use std::sync::{Arc, Mutex};

use super::tests::{crb, get, page, put, Capture, BASE, STARTUP};
use super::*;
use crate::backend::{RC_COMMAND_CODE, ST_NO_SESSIONS};

/// `TPM_LOC_STATE_x` with `locality` active.
fn assigned(locality: u8) -> u64 {
    0x81 | 0b10 | (u64::from(locality) << 2)
}

fn claim(crb: &mut TpmCrb, locality: u8) {
    crb.write(
        page(locality, LOC_CTRL),
        4,
        u64::from(LOC_CTRL_REQUEST_ACCESS),
    );
}

fn release(crb: &mut TpmCrb, locality: u8) {
    crb.write(page(locality, LOC_CTRL), 4, u64::from(LOC_CTRL_RELINQUISH));
}

/// `LOC_STS` of every locality, read through its own page.
fn statuses(crb: &mut TpmCrb) -> Vec<u64> {
    (0..LOCALITIES)
        .map(|l| crb.read(page(l, LOC_STS), 4))
        .collect()
}

#[test]
fn locality_claim_and_release_sequence() {
    let mut crb = crb();
    claim(&mut crb, 0);
    assert_eq!(crb.read(LOC_STATE, 4), assigned(0));
    assert_eq!(statuses(&mut crb), [1, 0, 0, 0, 0]);

    claim(&mut crb, 1);
    claim(&mut crb, 3);
    claim(&mut crb, 2);
    assert_eq!(
        crb.read(LOC_STATE, 4),
        assigned(0),
        "0 keeps it; 1, 2, 3 pend"
    );
    assert_eq!(statuses(&mut crb), [1, 0, 0, 0, 0]);

    release(&mut crb, 2);
    assert_eq!(
        crb.read(LOC_STATE, 4),
        assigned(0),
        "a pending release withdraws it"
    );

    release(&mut crb, 0);
    assert_eq!(
        crb.read(page(4, LOC_STATE), 4),
        assigned(3),
        "highest pending wins"
    );
    assert_eq!(statuses(&mut crb), [0, 0, 0, 1, 0]);

    release(&mut crb, 3);
    assert_eq!(crb.read(LOC_STATE, 4), assigned(1));
    assert_eq!(statuses(&mut crb), [0, 1, 0, 0, 0]);

    release(&mut crb, 1);
    assert_eq!(crb.read(LOC_STATE, 4), 0x81, "nothing assigned, 2 withdrew");
    assert_eq!(statuses(&mut crb), [0; 5]);

    claim(&mut crb, 0);
    crb.write(page(4, LOC_CTRL), 4, u64::from(LOC_CTRL_SEIZE));
    assert_eq!(crb.read(LOC_STATE, 4), assigned(4));
    assert_eq!(
        statuses(&mut crb),
        [u64::from(LOC_STS_BEEN_SEIZED), 0, 0, 0, 1]
    );
    claim(&mut crb, 0);
    release(&mut crb, 4);
    assert_eq!(crb.read(LOC_STATE, 4), assigned(0));
    assert_eq!(
        statuses(&mut crb),
        [1, 0, 0, 0, 0],
        "a grant clears beenSeized"
    );
}

type Calls = Arc<Mutex<Vec<(u8, Vec<u8>)>>>;

/// A backend that hands each command to a callback.
struct Callback<F>(F);

impl<F: FnMut(u8, &[u8]) -> Vec<u8> + Send> TpmBackend for Callback<F> {
    fn name(&self) -> &str {
        "callback"
    }

    fn execute(&mut self, locality: u8, command: &[u8]) -> Vec<u8> {
        (self.0)(locality, command)
    }
}

/// A CRB whose backend records every call and answers with `response`.
fn recording(response: Vec<u8>) -> (TpmCrb, Calls) {
    let calls = Calls::default();
    let log = Arc::clone(&calls);
    let backend = Callback(move |locality: u8, command: &[u8]| {
        log.lock()
            .expect("calls")
            .push((locality, command.to_vec()));
        response.clone()
    });
    (TpmCrb::new(BASE, Box::new(backend)).expect("crb"), calls)
}

#[test]
fn command_buffer_write_then_start_bit_triggers_the_callback() {
    let (mut crb, calls) = recording(error_response(ST_NO_SESSIONS, RC_COMMAND_CODE));
    claim(&mut crb, 2);
    crb.write(page(2, CTRL_REQ), 4, u64::from(CTRL_REQ_CMD_READY));
    let mut staged = STARTUP.to_vec();
    staged.extend_from_slice(&[0xee; 4]);
    put(&mut crb, 2, &staged);
    assert!(
        calls.lock().expect("calls").is_empty(),
        "buffer writes run nothing"
    );

    crb.write(page(2, CTRL_START), 4, 0);
    crb.write(page(2, CTRL_CANCEL), 4, 1);
    assert!(
        calls.lock().expect("calls").is_empty(),
        "START=0 runs nothing"
    );

    crb.write(page(2, CTRL_START), 4, u64::from(START_INVOKE));
    assert_eq!(
        *calls.lock().expect("calls"),
        [(2, STARTUP.to_vec())],
        "one call, commandSize bytes, the active locality"
    );
    assert_eq!(crb.read(page(2, CTRL_START), 4), 0, "START is complete");
    assert_eq!(crb.commands(), 1);

    put(&mut crb, 2, &STARTUP);
    crb.write(page(2, CTRL_START), 1, u64::from(START_INVOKE));
    assert_eq!(calls.lock().expect("calls").len(), 2, "byte write to START");
}

#[test]
fn response_buffer_reads_back_at_every_width() {
    let mut response = vec![0x80, 0x01, 0, 0, 0, 26, 0, 0, 0, 0];
    response.extend(0x10u8..0x20);
    let (mut crb, _) = recording(response.clone());
    claim(&mut crb, 0);
    put(&mut crb, 0, &STARTUP);
    crb.write(CTRL_START, 4, u64::from(START_INVOKE));

    assert_eq!(get(&mut crb, 0, response.len()), response, "byte reads");
    for size in [2u8, 4, 8] {
        let words: Vec<u8> = (0..response.len() as u64 / u64::from(size))
            .flat_map(|i| {
                let value = crb.read(DATA_BUFFER + i * u64::from(size), size);
                value.to_le_bytes()[..usize::from(size)].to_vec()
            })
            .collect();
        assert_eq!(words, response[..words.len()], "{size}-byte reads");
    }
    assert_eq!(
        get(&mut crb, 3, response.len()),
        response,
        "every page shows the one buffer"
    );
    assert_eq!(crb.read(RSP_ADDR_LO, 8), BASE + DATA_BUFFER);
    assert_eq!(crb.read(RSP_SIZE, 4), DATA_BUFFER_SIZE as u64);
}

/// EDK2 `Tpm2Ptp.c` (`PtpCrbRequestUseTpm`, `PtpCrbTpmCommand`) at locality 0.
#[test]
fn edk2_ptp_crb_command_flow_completes_without_warnings() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let response = tracing::subscriber::with_default(subscriber, || {
        let mut crb = crb();
        let id = crb.read(INTF_ID_LO, 4);
        assert_eq!((id & 0xf, id >> 4 & 0xf, id >> 14 & 1), (1, 1, 1), "CRB");
        claim(&mut crb, 0);
        assert_eq!(crb.read(LOC_STS, 4) & 1, 1, "granted");
        assert_eq!(crb.read(CTRL_STS, 4), u64::from(CTRL_STS_IDLE));
        crb.write(CTRL_REQ, 4, u64::from(CTRL_REQ_CMD_READY));
        assert_eq!(crb.read(CTRL_REQ, 4), 0, "cmdReady cleared");
        assert_eq!(crb.read(CTRL_STS, 4), 0, "tpmIdle cleared");
        for (i, byte) in STARTUP.iter().enumerate() {
            crb.write(DATA_BUFFER + i as u64, 1, u64::from(*byte));
        }
        let buffer = BASE + DATA_BUFFER;
        crb.write(CMD_HADDR, 4, buffer >> 32);
        crb.write(CMD_LADDR, 4, buffer & 0xffff_ffff);
        crb.write(CMD_SIZE, 4, DATA_BUFFER_SIZE as u64);
        crb.write(RSP_ADDR_LO, 8, buffer & 0xffff_ffff);
        crb.write(RSP_SIZE, 4, DATA_BUFFER_SIZE as u64);
        crb.write(CTRL_START, 4, u64::from(START_INVOKE));
        assert_eq!(crb.read(CTRL_START, 4), 0);
        let response = get(&mut crb, 0, HEADER_LEN);
        crb.write(CTRL_REQ, 4, u64::from(CTRL_REQ_GO_IDLE));
        assert_eq!(crb.read(CTRL_STS, 4), u64::from(CTRL_STS_IDLE));
        response
    });
    assert_eq!(response, error_response(ST_NO_SESSIONS, RC_COMMAND_CODE));
    let text = String::from_utf8(capture.0.lock().expect("capture").clone()).expect("utf8");
    assert_eq!(text, "", "EDK2's flow logs no WARN");
}
