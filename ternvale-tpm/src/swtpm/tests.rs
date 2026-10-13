//! swtpm backend pieces without the swtpm binary: capability parsing, the
//! state-file header check, control framing, the argument list, and the
//! backend against a fake swtpm on real Unix sockets.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::backend::SwtpmBackend;
use super::ctrl::{self, Control};
use super::locate::parse_capabilities;
use super::process::{option_path, swtpm_args};
use super::state::{parse_prefix, Prefix};
use super::*;
use crate::backend::{error_response, TpmBackend, RC_FAILURE, ST_NO_SESSIONS};

/// swtpm 0.10.2 (Homebrew) `socket --print-capabilities`.
const CAPS_0_10_2: &str = r#"{ "type": "swtpm", "features": [ "tpm-1.2", "tpm-2.0", "tpm-send-command-header", "flags-opt-startup", "flags-opt-disable-auto-shutdown", "ctrl-opt-terminate", "cmdarg-key-fd", "cmdarg-pwd-fd", "cmdarg-print-states", "cmdarg-chroot", "cmdarg-migration", "nvram-backend-dir", "nvram-backend-file", "cmdarg-print-info", "tpmstate-opt-lock" ], "profiles": { }, "version": "0.10.2" }"#;
/// The first 12 bytes of a state file swtpm 0.10.2 wrote.
const HEADER_0_10_2: [u8; 12] = [
    0x6e, 0x69, 0x6c, 0x6d, 0x70, 0x74, 0x77, 0x73, 0x01, 0x00, 0xc0, 0x00,
];
/// `TPM2_Startup(TPM_SU_CLEAR)`.
const STARTUP: [u8; 12] = [0x80, 0x01, 0, 0, 0, 12, 0, 0, 0x01, 0x44, 0, 0];

pub(super) fn scratch(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ternvale-tpm-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn capabilities_parse_and_list_what_ternvale_needs() {
    let (version, features) = parse_capabilities(CAPS_0_10_2).expect("parse");
    assert_eq!(version, "0.10.2");
    for feature in REQUIRED_FEATURES {
        assert!(features.iter().any(|f| f == feature), "{feature}");
    }
    let (version, features) =
        parse_capabilities(r#"{ "type": "swtpm", "features": [ "cmdarg-seccomp" ] }"#)
            .expect("pre-0.7 output");
    assert_eq!(
        (version.as_str(), features),
        ("unknown", vec!["cmdarg-seccomp".into()])
    );
    assert_eq!(parse_capabilities("usage: swtpm ..."), None);
}

#[test]
fn state_header_needs_magic_known_version_and_sane_size() {
    assert_eq!(parse_prefix(&HEADER_0_10_2, 8384), Ok(()));
    let mut bad = HEADER_0_10_2;
    bad[0] ^= 1;
    assert!(matches!(parse_prefix(&bad, 8384), Err(Prefix::Other(r)) if r.contains("swtpmlin")));
    let mut newer = HEADER_0_10_2;
    newer[8] = 2;
    assert_eq!(parse_prefix(&newer, 8384), Err(Prefix::Version(2)));
    assert!(
        matches!(parse_prefix(&HEADER_0_10_2, 100), Err(Prefix::Other(r)) if r.contains("header size 192"))
    );
}

#[test]
fn state_file_check_accepts_new_and_valid_files_only() {
    let dir = scratch("state");
    let path = dir.join("nvram.fd.swtpm");
    assert_eq!(check_state_file(&path), Ok(StateFile::New));
    let orphan = dir.join("missing-dir/nvram.fd.swtpm");
    assert!(
        matches!(check_state_file(&orphan), Err(SwtpmError::State { reason, .. }) if reason.contains("directory"))
    );

    std::fs::write(&path, b"").expect("write");
    assert!(
        matches!(check_state_file(&path), Err(SwtpmError::State { reason, .. }) if reason.contains("0 bytes"))
    );
    let mut file = HEADER_0_10_2.to_vec();
    file.resize(8384, 0);
    std::fs::write(&path, &file).expect("write");
    assert_eq!(
        check_state_file(&path),
        Ok(StateFile::Existing { bytes: 8384 })
    );
    file[8] = 7;
    std::fs::write(&path, &file).expect("write");
    let error = check_state_file(&path).expect_err("version 7");
    assert_eq!(
        error,
        SwtpmError::StateVersion {
            path: path.clone(),
            found: 7,
            supported: STATE_VERSION
        }
    );
    assert!(
        error.to_string().contains("only knows version 1"),
        "{error}"
    );
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn control_requests_are_big_endian_code_then_body() {
    assert_eq!(ctrl::encode(ctrl::CMD_SET_LOCALITY, &[2]), [0, 0, 0, 5, 2]);
    assert_eq!(
        ctrl::encode(ctrl::CMD_SET_BUFFERSIZE, &3968u32.to_be_bytes()),
        [0, 0, 0, 0x11, 0, 0, 0x0f, 0x80]
    );
    assert_eq!(ctrl::command_name(ctrl::CMD_INIT), "CMD_INIT");
}

#[test]
fn invocation_matches_docs_tpm_md() {
    assert_eq!(
        swtpm_args("/v/nvram.fd.swtpm", "/t/data.sock", "/t/ctrl.sock").join(" "),
        "socket --tpm2 --tpmstate backend-uri=file:///v/nvram.fd.swtpm,lock \
         --server type=unixio,path=/t/data.sock --ctrl type=unixio,path=/t/ctrl.sock,terminate"
    );
    assert!(matches!(
        option_path("tpm state file", std::path::Path::new("/v/a,b.swtpm")),
        Err(SwtpmError::Path {
            reason: "contains ','",
            ..
        })
    ));
    let relative = option_path("tpm state file", std::path::Path::new("x.swtpm")).expect("path");
    assert!(
        relative.starts_with('/') && relative.ends_with("/x.swtpm"),
        "{relative}"
    );
}

#[test]
fn a_configured_binary_must_be_executable() {
    let dir = scratch("locate");
    let plain = dir.join("swtpm");
    std::fs::write(&plain, b"#!/bin/sh\n").expect("write");
    assert!(matches!(
        locate_swtpm(Some(&plain)),
        Err(SwtpmError::NotFound { .. })
    ));
    assert!(matches!(
        locate_swtpm(Some(&dir.join("nope"))),
        Err(SwtpmError::NotFound { .. })
    ));
    assert!(SwtpmError::NotFound {
        searched: String::new()
    }
    .is_unavailable());
    assert!(!SwtpmError::Data {
        reason: String::new()
    }
    .is_unavailable());
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

/// What the fake swtpm received, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Seen {
    Ctrl(u32, Vec<u8>),
    Data(Vec<u8>),
}

pub(super) type Log = Arc<Mutex<Vec<Seen>>>;

/// A fake swtpm: answers control commands with `caps` and a 3968-byte
/// buffer, and data commands with `respond(command)`.
pub(super) fn fake_swtpm(
    caps: u32,
    respond: impl Fn(&[u8]) -> Vec<u8> + Send + 'static,
) -> (SwtpmBackend, Log, Vec<JoinHandle<()>>) {
    let (ctrl_client, mut ctrl_server) = UnixStream::pair().expect("ctrl pair");
    let (data_client, mut data_server) = UnixStream::pair().expect("data pair");
    let log: Log = Arc::default();
    let ctrl_log = Arc::clone(&log);
    let ctrl_thread = std::thread::spawn(move || {
        let mut code = [0u8; 4];
        while ctrl_server.read_exact(&mut code).is_ok() {
            let code = u32::from_be_bytes(code);
            let body_len = match code {
                ctrl::CMD_INIT | ctrl::CMD_SET_BUFFERSIZE => 4,
                ctrl::CMD_SET_LOCALITY => 1,
                _ => 0,
            };
            let mut body = vec![0; body_len];
            ctrl_server.read_exact(&mut body).expect("ctrl body");
            ctrl_log.lock().expect("log").push(Seen::Ctrl(code, body));
            let reply: Vec<u8> = match code {
                ctrl::CMD_GET_CAPABILITY => {
                    [0u32, caps].iter().flat_map(|w| w.to_be_bytes()).collect()
                }
                ctrl::CMD_SET_BUFFERSIZE => [0u32, 3968, 2808, 4096]
                    .iter()
                    .flat_map(|w| w.to_be_bytes())
                    .collect(),
                _ => vec![0; 4],
            };
            ctrl_server.write_all(&reply).expect("ctrl reply");
        }
    });
    let data_log = Arc::clone(&log);
    let data_thread = std::thread::spawn(move || {
        let mut header = [0u8; 10];
        while data_server.read_exact(&mut header).is_ok() {
            let size = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
            let mut command = header.to_vec();
            command.resize(size.max(10), 0);
            data_server
                .read_exact(&mut command[10..])
                .expect("command body");
            data_log
                .lock()
                .expect("log")
                .push(Seen::Data(command.clone()));
            if data_server.write_all(&respond(&command)).is_err() {
                break;
            }
        }
    });
    let control = Control::new(ctrl_client).expect("control");
    let backend = SwtpmBackend::from_parts(control, data_client, None).expect("backend");
    (backend, log, vec![ctrl_thread, data_thread])
}

/// A success response carrying `command` as its parameters, so a test can
/// see the exact bytes swtpm got back in the reply.
pub(super) fn echo(command: &[u8]) -> Vec<u8> {
    let mut response = vec![0x80, 0x01, 0, 0, 0, 0, 0, 0, 0, 0];
    response.extend_from_slice(command);
    let len = response.len() as u32;
    response[2..6].copy_from_slice(&len.to_be_bytes());
    response
}

#[test]
fn start_up_sets_the_crb_buffer_size_before_init() {
    let (mut backend, log, threads) = fake_swtpm(0x1ffff, echo);
    backend.initialize(3968).expect("initialize");
    assert_eq!(
        *log.lock().expect("log"),
        [
            Seen::Ctrl(ctrl::CMD_GET_CAPABILITY, vec![]),
            Seen::Ctrl(ctrl::CMD_SET_BUFFERSIZE, 3968u32.to_be_bytes().to_vec()),
            Seen::Ctrl(ctrl::CMD_INIT, vec![0; 4]),
        ]
    );
    drop(backend);
    threads.into_iter().for_each(|t| t.join().expect("fake"));
}

#[test]
fn start_up_refuses_a_swtpm_without_set_locality() {
    let (mut backend, _log, _threads) = fake_swtpm(0x1ffff & !(1 << 3), echo);
    let error = backend.initialize(3968).expect_err("no CMD_SET_LOCALITY");
    assert!(error.to_string().contains("lack 0x8"), "{error}");
}

#[test]
fn commands_and_responses_pass_through_unmodified_with_locality_switches() {
    let (mut backend, log, threads) = fake_swtpm(0x1ffff, echo);
    assert!(backend.host_io());
    assert_eq!(backend.execute(0, &STARTUP), echo(&STARTUP));
    let mut odd = STARTUP;
    odd[10..].copy_from_slice(&[0xde, 0xad]);
    assert_eq!(backend.execute(0, &odd), echo(&odd));
    assert_eq!(backend.execute(3, &STARTUP), echo(&STARTUP));
    assert_eq!(
        *log.lock().expect("log"),
        [
            Seen::Ctrl(ctrl::CMD_SET_LOCALITY, vec![0]),
            Seen::Data(STARTUP.to_vec()),
            Seen::Data(odd.to_vec()),
            Seen::Ctrl(ctrl::CMD_SET_LOCALITY, vec![3]),
            Seen::Data(STARTUP.to_vec()),
        ]
    );
    drop(backend);
    threads.into_iter().for_each(|t| t.join().expect("fake"));
}

#[test]
fn a_malformed_response_puts_the_backend_in_failure_mode() {
    let (mut backend, log, _threads) =
        fake_swtpm(0x1ffff, |_| vec![0x80, 0x01, 0, 0, 0, 4, 0, 0, 0, 0]);
    let failure = error_response(ST_NO_SESSIONS, RC_FAILURE);
    assert_eq!(backend.execute(0, &STARTUP), failure);
    assert_eq!(backend.execute(0, &STARTUP), failure);
    let data = log
        .lock()
        .expect("log")
        .iter()
        .filter(|s| matches!(s, Seen::Data(_)))
        .count();
    assert_eq!(data, 1, "failure mode never touches the socket again");
}
