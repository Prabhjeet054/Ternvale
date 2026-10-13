//! Commands through the CRB land in its [`TpmHistory`], and secrets stay out
//! of both the history and the TRACE log.

use super::tests::{run, BASE, STARTUP};
use super::*;
use crate::backend::{hex, ST_NO_SESSIONS};

/// `TPM2_GetRandom(16)`.
const GET_RANDOM: [u8; 12] = [0x80, 0x01, 0, 0, 0, 12, 0, 0, 0x01, 0x7b, 0, 16];
/// Bytes a guest might turn into a key.
const RANDOM: [u8; 16] = [
    0xc0, 0xff, 0xee, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
];

/// Answers GetRandom with [`RANDOM`] and everything else with success.
struct Fake;

impl TpmBackend for Fake {
    fn name(&self) -> &str {
        "fake"
    }

    fn execute(&mut self, _locality: u8, command: &[u8]) -> Vec<u8> {
        let mut response = vec![0x80, 0x01, 0, 0, 0, 10, 0, 0, 0, 0];
        if command.get(6..10) == Some(&[0, 0, 0x01, 0x7b]) {
            response.extend_from_slice(&(RANDOM.len() as u16).to_be_bytes());
            response.extend_from_slice(&RANDOM);
            let len = response.len() as u32;
            response[2..6].copy_from_slice(&len.to_be_bytes());
        }
        response
    }
}

#[test]
fn history_keeps_public_bytes_and_only_codes_and_sizes_for_secrets() {
    let (history, log) = crate::test_log::capture(|| {
        let mut crb = TpmCrb::new(BASE, Box::new(Fake)).expect("crb");
        let history = crb.history();
        assert_eq!(run(&mut crb, 0, &STARTUP).len(), 10);
        assert_eq!(run(&mut crb, 0, &GET_RANDOM).len(), 28);
        history
    });
    assert_eq!(history.total(), 2);
    assert_eq!(history.backend(), "fake");
    let [startup, random] = history.exchanges().try_into().expect("two exchanges");
    assert_eq!(
        (
            startup.seq,
            startup.code,
            startup.command_len,
            startup.rc,
            startup.response_len
        ),
        (1, Some(0x144), 12, 0, 10)
    );
    assert_eq!(startup.tag, Some(ST_NO_SESSIONS));
    assert_eq!(startup.command.as_deref(), Some(&STARTUP[..]));
    assert_eq!(startup.response.as_ref().map(Vec::len), Some(10));
    assert_eq!(
        (
            random.seq,
            random.code,
            random.command_len,
            random.rc,
            random.response_len
        ),
        (2, Some(0x17b), 12, 0, 28)
    );
    assert_eq!(
        (random.command.as_ref(), random.response.as_ref()),
        (None, None)
    );

    let text = history.render();
    let secret = hex(&RANDOM);
    assert!(!text.contains(&secret), "{text}");
    assert!(
        text.contains("name=GetRandom cmd_len=12 rc=0x0 rsp_len=28"),
        "{text}"
    );
    assert!(text.contains("bytes=redacted"), "{text}");
    assert!(text.contains("cmd=80010000000c000001440000"), "{text}");

    assert!(
        !log.contains(&secret),
        "random bytes in the TRACE log:\n{log}"
    );
    let random_lines: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("code=0x17b") || l.contains("len=28"))
        .filter(|l| l.contains("tpm command locality=") || l.contains("tpm response locality="))
        .collect();
    assert_eq!(random_lines.len(), 2, "{log}");
    assert!(
        random_lines.iter().all(|l| l.contains("bytes=redacted")),
        "{log}"
    );
    assert!(log.contains("bytes=80010000000c000001440000"), "{log}");
}

#[test]
fn history_is_shared_and_bounded() {
    let mut crb = TpmCrb::new(BASE, Box::new(Fake)).expect("crb");
    let history = crb.history();
    for _ in 0..TPM_HISTORY_LEN + 5 {
        run(&mut crb, 0, &STARTUP);
    }
    assert_eq!(history.total(), (TPM_HISTORY_LEN + 5) as u64);
    let kept = crb.history().exchanges();
    assert_eq!(kept.len(), TPM_HISTORY_LEN);
    assert_eq!(kept[0].seq, 6);
    assert_eq!(kept[TPM_HISTORY_LEN - 1].seq, (TPM_HISTORY_LEN + 5) as u64);
}
