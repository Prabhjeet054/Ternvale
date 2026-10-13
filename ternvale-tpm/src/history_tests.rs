use super::*;
use crate::backend::{ST_NO_SESSIONS, ST_SESSIONS};

fn command(tag: u16, code: u32, len: usize) -> (Option<CommandHeader>, Vec<u8>) {
    let mut bytes = vec![0x5a; len];
    bytes[..2].copy_from_slice(&tag.to_be_bytes());
    bytes[2..6].copy_from_slice(&(len as u32).to_be_bytes());
    bytes[6..10].copy_from_slice(&code.to_be_bytes());
    (CommandHeader::parse(&bytes), bytes)
}

fn response(rc: u32, len: usize) -> Vec<u8> {
    let mut bytes = vec![0xa5; len];
    bytes[..2].copy_from_slice(&ST_NO_SESSIONS.to_be_bytes());
    bytes[2..6].copy_from_slice(&(len as u32).to_be_bytes());
    bytes[6..10].copy_from_slice(&rc.to_be_bytes());
    bytes
}

#[test]
fn an_empty_history_says_so() {
    let history = TpmHistory::new("swtpm", 4);
    assert_eq!(history.total(), 0);
    let text = history.render();
    assert!(
        text.starts_with("# TPM commands: backend swtpm, 0 since"),
        "{text}"
    );
    assert!(text.contains("# no TPM commands yet"), "{text}");
}

#[test]
fn sessions_redact_even_a_public_command() {
    let history = TpmHistory::new("swtpm", 4);
    let (header, bytes) = command(ST_SESSIONS, 0x17A, 40);
    history.record(0, header, &bytes, &response(0, 30), 7);
    let [exchange] = history.exchanges().try_into().expect("one");
    assert_eq!((exchange.command, exchange.response), (None, None));
    assert_eq!(exchange.tag, Some(ST_SESSIONS));
    assert!(history
        .render()
        .contains("name=GetCapability cmd_len=40 rc=0x0 rsp_len=30 us=7 bytes=redacted"));
}

#[test]
fn long_public_responses_are_cut_and_counted() {
    let history = TpmHistory::new("swtpm", 4);
    let (header, bytes) = command(ST_NO_SESSIONS, 0x17A, 22);
    history.record(2, header, &bytes, &response(0, 1000), 15);
    let [exchange] = history.exchanges().try_into().expect("one");
    assert_eq!(
        exchange.response.as_ref().map(Vec::len),
        Some(TPM_HISTORY_BYTES)
    );
    assert_eq!(exchange.command.as_deref(), Some(bytes.as_slice()));
    let text = history.render();
    assert!(text.contains("locality=2"), "{text}");
    assert!(
        text.contains(&format!("(+{} more bytes)", 1000 - TPM_HISTORY_BYTES)),
        "{text}"
    );
}

#[test]
fn error_responses_and_unparsable_commands_are_recorded() {
    let history = TpmHistory::new("stub", 4);
    let (header, bytes) = command(ST_NO_SESSIONS, 0x131, 200);
    history.record(0, header, &bytes, &response(0x9a2, 10), 3);
    history.record(0, None, &[0; 4], &response(0x95, 10), 1);
    let text = history.render();
    assert!(
        text.contains("code=0x131 name=CreatePrimary cmd_len=200 rc=0x9a2 rsp_len=10"),
        "{text}"
    );
    assert!(
        text.contains("code=none name=unknown cmd_len=0 rc=0x95"),
        "{text}"
    );
    assert!(
        !text.contains("5a5a"),
        "CreatePrimary bytes leaked:\n{text}"
    );
    assert_eq!(text.matches("bytes=redacted").count(), 2, "{text}");
}

#[test]
fn the_ring_drops_the_oldest_and_keeps_counting() {
    let history = TpmHistory::new("swtpm", 3);
    let (header, bytes) = command(ST_NO_SESSIONS, 0x144, 12);
    for _ in 0..5 {
        history.record(0, header, &bytes, &response(0, 10), 1);
    }
    let seqs: Vec<u64> = history.exchanges().iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [3, 4, 5]);
    assert_eq!(history.total(), 5);
    let text = history.render();
    assert!(
        text.contains("5 since the VM started, the last 3 below"),
        "{text}"
    );
    assert_eq!(TpmHistory::new("x", 0).capacity, 1);
}

#[test]
fn a_backend_failure_names_the_first_failed_command_in_the_header() {
    let history = TpmHistory::new("swtpm", 4);
    assert!(!history.render().contains(BACKEND_FAILED));
    let (header, bytes) = command(ST_NO_SESSIONS, 0x144, 12);
    history.record(0, header, &bytes, &response(0, 10), 5);
    history.note_failure("swtpm (pid 7) exited with signal: 9 (SIGKILL)");
    history.note_failure("a later failure is not kept");
    let (header, bytes) = command(ST_NO_SESSIONS, 0x143, 11);
    history.record(0, header, &bytes, &response(0x101, 10), 5);
    let failure = history.failure().expect("failure");
    assert_eq!(failure.seq, 2);
    assert_eq!(
        failure.reason,
        "swtpm (pid 7) exited with signal: 9 (SIGKILL)"
    );
    let text = history.render();
    let line = text.lines().nth(1).expect("second header line").to_string();
    assert!(
        line.starts_with("# backend failed: swtpm (pid 7) exited with signal: 9 (SIGKILL); seq=2 and every later command got TPM_RC_FAILURE (rc=0x101) from Ternvale"),
        "{text}"
    );
    assert!(!text.contains("a later failure"), "{text}");
    assert!(
        text.contains("seq=2 ") && text.contains("rc=0x101"),
        "{text}"
    );
}
