//! A backend with host I/O runs on a worker thread: `START` reads 1 until it
//! answers, the buffer is protected meanwhile, and the response is collected
//! on the next register access.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use super::tests::{get, page, put, BASE, STARTUP};
use super::*;
use crate::backend::{HEADER_LEN, RC_COMMAND_CODE, ST_NO_SESSIONS};

/// Answers each command once the test sends on its gate, and records what
/// it got and whether it was dropped.
struct Gated {
    gate: Receiver<Vec<u8>>,
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
    dropped: Arc<AtomicBool>,
}

impl TpmBackend for Gated {
    fn name(&self) -> &str {
        "gated"
    }

    fn execute(&mut self, _locality: u8, command: &[u8]) -> Vec<u8> {
        self.seen.lock().expect("seen").push(command.to_vec());
        self.gate.recv().unwrap_or_default()
    }

    fn host_io(&self) -> bool {
        true
    }
}

impl Drop for Gated {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

type Seen = Arc<Mutex<Vec<Vec<u8>>>>;

fn gated() -> (TpmCrb, Sender<Vec<u8>>, Seen, Arc<AtomicBool>) {
    let (open, gate) = mpsc::channel();
    let seen: Seen = Arc::default();
    let dropped = Arc::new(AtomicBool::new(false));
    let backend = Gated {
        gate,
        seen: Arc::clone(&seen),
        dropped: Arc::clone(&dropped),
    };
    let crb = TpmCrb::new(BASE, Box::new(backend)).expect("crb");
    (crb, open, seen, dropped)
}

fn start(crb: &mut TpmCrb, command: &[u8]) {
    crb.write(page(0, LOC_CTRL), 4, u64::from(LOC_CTRL_REQUEST_ACCESS));
    crb.write(page(0, CTRL_REQ), 4, u64::from(CTRL_REQ_CMD_READY));
    put(crb, 0, command);
    crb.write(page(0, CTRL_START), 4, u64::from(START_INVOKE));
}

/// Poll `START` like a driver until it reads 0.
fn wait_for_start_clear(crb: &mut TpmCrb) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while crb.read(page(0, CTRL_START), 4) != 0 {
        assert!(std::time::Instant::now() < deadline, "START never cleared");
        std::thread::yield_now();
    }
}

#[test]
fn start_reads_one_until_the_worker_answers() {
    let (mut crb, open, seen, _dropped) = gated();
    start(&mut crb, &STARTUP);
    for _ in 0..3 {
        assert_eq!(crb.read(page(0, CTRL_START), 4), u64::from(START_INVOKE));
    }
    assert_eq!(crb.commands(), 0);
    let response = vec![0x80, 0x01, 0, 0, 0, 12, 0, 0, 0, 0, 0xab, 0xcd];
    open.send(response.clone()).expect("open gate");
    wait_for_start_clear(&mut crb);
    assert_eq!(get(&mut crb, 0, response.len()), response);
    assert_eq!(crb.commands(), 1);
    assert_eq!(*seen.lock().expect("seen"), [STARTUP.to_vec()]);
}

#[test]
fn buffer_and_start_writes_are_ignored_while_a_command_runs() {
    let (mut crb, open, seen, _dropped) = gated();
    start(&mut crb, &STARTUP);
    crb.write(page(0, DATA_BUFFER), 4, 0xffff_ffff);
    crb.write(page(0, CTRL_START), 4, u64::from(START_INVOKE));
    assert_eq!(get(&mut crb, 0, STARTUP.len()), STARTUP, "buffer unchanged");
    open.send(error_response_bytes()).expect("open gate");
    wait_for_start_clear(&mut crb);
    assert_eq!(
        seen.lock().expect("seen").len(),
        1,
        "the second START was dropped"
    );
    assert_eq!(get(&mut crb, 0, HEADER_LEN), error_response_bytes());
}

#[test]
fn a_malformed_worker_response_becomes_tpm_rc_failure() {
    let (mut crb, open, _seen, _dropped) = gated();
    start(&mut crb, &STARTUP);
    open.send(vec![0x80, 0x01, 0, 0, 0, 99]).expect("open gate");
    wait_for_start_clear(&mut crb);
    let rc = get(&mut crb, 0, HEADER_LEN)[6..].to_vec();
    assert_eq!(rc, crate::backend::RC_FAILURE.to_be_bytes());
}

#[test]
fn dropping_the_crb_stops_the_worker_and_drops_the_backend() {
    let (crb, _open, _seen, dropped) = gated();
    drop(crb);
    assert!(dropped.load(Ordering::SeqCst));
}

fn error_response_bytes() -> Vec<u8> {
    crate::backend::error_response(ST_NO_SESSIONS, RC_COMMAND_CODE)
}
