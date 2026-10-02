use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_agent::{run, Actions, AgentConfig, AgentError, Backoff, Exit, Target};
use ternvale_agent_proto::{read_message, write_message, ClipboardText, Message};

use super::*;

fn socket(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("tvas-{}-{name}.sock", std::process::id()));
    if let Err(error) = std::fs::remove_file(&path) {
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error}");
    }
    path
}

fn fast() -> AgentServerConfig {
    AgentServerConfig {
        heartbeat: Duration::from_millis(100),
        pong_timeout: Duration::from_millis(400),
        hello_timeout: Duration::from_millis(400),
        poll: Duration::from_millis(10),
    }
}

const WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<String>>>);

impl Actions for Recorder {
    fn set_resolution(&mut self, width: u32, height: u32) -> Result<(), AgentError> {
        let mut calls = self.0.lock().expect("recorder");
        calls.push(format!("resolution {width}x{height}"));
        Ok(())
    }
    fn clipboard_set(&mut self, text: &str) -> Result<(), AgentError> {
        let mut calls = self.0.lock().expect("recorder");
        calls.push(format!("clipboard {text}"));
        Ok(())
    }
    fn shutdown(&mut self) -> Result<(), AgentError> {
        self.0.lock().expect("recorder").push("shutdown".into());
        Ok(())
    }
}

/// Connect a fake agent and say hello with `version`.
fn hello(path: &std::path::Path, version: u32) -> UnixStream {
    let mut stream = UnixStream::connect(path).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let message = Message::Hello {
        version,
        os: "TestOS 1.0 aarch64".into(),
        agent: "fake".into(),
    };
    write_message(&mut stream, &message, "host").expect("hello");
    stream
}

#[test]
fn real_agent_connects_heartbeats_reconnects_and_shuts_down() {
    let path = socket("real");
    let server = AgentServer::start_with(&path, "unit", fast()).expect("start");
    assert_eq!(server.status().state, AgentState::Waiting);

    let stop = Arc::new(AtomicBool::new(false));
    let recorder = Recorder::default();
    let config = AgentConfig {
        backoff: Backoff::new(Duration::from_millis(20), Duration::from_millis(200)),
        poll: Duration::from_millis(10),
        idle_timeout: Duration::from_secs(2),
        ..AgentConfig::new(Target::Unix(path.clone()))
    };
    let (flag, mut actions) = (Arc::clone(&stop), recorder.clone());
    let agent = std::thread::spawn(move || run(&config, &mut actions, &flag));

    let status = server
        .wait_for(WAIT, |s| s.state == AgentState::Connected && s.pongs >= 2)
        .expect("agent connected and answering pings");
    assert_eq!(status.version, Some(1));
    assert!(status
        .agent
        .as_deref()
        .is_some_and(|a| a.starts_with("ternvale-agent ")));
    assert!(status
        .os
        .as_deref()
        .is_some_and(|os| os.contains(std::env::consts::ARCH)));
    assert!(status.rtt_us.is_some() && status.last_pong_ms.is_some());
    assert_eq!((status.connects, status.disconnects), (1, 0));

    server
        .send(&Message::ClipboardSet {
            text: ClipboardText("from host".into()),
        })
        .expect("clipboard");
    server
        .send(&Message::SetResolution {
            width: 1024,
            height: 768,
        })
        .expect("resolution");

    drop(server);
    let server = AgentServer::start_with(&path, "unit", fast()).expect("restart");
    let restarted = Instant::now();
    let status = server
        .wait_for(WAIT, |s| s.state == AgentState::Connected)
        .expect("agent reconnected after the host restarted");
    assert_eq!(status.connects, 1);
    assert!(restarted.elapsed() < Duration::from_secs(2));

    server.send(&Message::Shutdown).expect("shutdown");
    assert_eq!(agent.join().expect("agent thread"), Exit::Shutdown);
    let status = server
        .wait_for(WAIT, |s| s.state == AgentState::Disconnected)
        .expect("disconnect seen");
    assert_eq!(status.disconnects, 1);
    assert_eq!(
        *recorder.0.lock().expect("recorder"),
        ["clipboard from host", "resolution 1024x768", "shutdown"]
    );
    stop.store(true, Ordering::Release);
}

#[test]
fn old_agent_is_rejected_with_the_supported_range() {
    let path = socket("reject");
    let server = AgentServer::start_with(&path, "unit", fast()).expect("start");
    let mut agent = hello(&path, 0);
    match read_message(&mut agent, "host").expect("reply") {
        Message::Reject {
            reason,
            min_version,
            max_version,
        } => {
            assert_eq!((min_version, max_version), (1, 1));
            assert!(reason.contains("version 0 is not supported"), "{reason}");
        }
        other => panic!("expected reject, got {other:?}"),
    }
    let status = server
        .wait_for(WAIT, |s| s.last_error.is_some())
        .expect("recorded");
    assert_eq!(status.state, AgentState::Waiting);
    assert!(status
        .last_error
        .as_deref()
        .is_some_and(|e| e.starts_with("rejected:")));
}

#[test]
fn silent_and_unresponsive_agents_are_dropped() {
    let path = socket("silent");
    let server = AgentServer::start_with(&path, "unit", fast()).expect("start");
    let _silent = UnixStream::connect(&path).expect("connect");
    let status = server
        .wait_for(WAIT, |s| s.last_error.is_some())
        .expect("hello timeout");
    assert_eq!(status.last_error.as_deref(), Some("no hello within 400 ms"));

    let mut deaf = hello(&path, 1);
    assert_eq!(
        read_message(&mut deaf, "host").expect("welcome"),
        Message::Welcome { version: 1 }
    );
    assert!(matches!(
        read_message(&mut deaf, "host"),
        Ok(Message::Ping { seq: 1 })
    ));
    let status = server
        .wait_for(WAIT, |s| s.state == AgentState::Disconnected)
        .expect("pong timeout");
    assert_eq!(status.last_error.as_deref(), Some("no pong within 400 ms"));
    assert_eq!(
        (status.connects, status.disconnects, status.pongs),
        (1, 1, 0)
    );
}

#[test]
fn newer_connection_replaces_the_current_one_and_unknown_types_are_skipped() {
    let path = socket("replace");
    let server = AgentServer::start_with(&path, "unit", fast()).expect("start");
    let mut first = hello(&path, 1);
    assert!(matches!(
        read_message(&mut first, "host"),
        Ok(Message::Welcome { .. })
    ));
    server
        .wait_for(WAIT, |s| s.state == AgentState::Connected)
        .expect("first");

    let mut second = hello(&path, 1);
    let unknown = br#"{"type":"from_the_future"}"#;
    let mut frame = (unknown.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(unknown);
    second.write_all(&frame).expect("unknown");
    write_message(&mut second, &Message::Ping { seq: 42 }, "host").expect("ping");
    let mut got_pong = false;
    while !got_pong {
        match read_message(&mut second, "host").expect("reply") {
            Message::Welcome { .. } | Message::Ping { .. } => {}
            Message::Pong { seq } => {
                assert_eq!(seq, 42);
                got_pong = true;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    let status = server.status();
    assert_eq!(status.state, AgentState::Connected);
    assert_eq!((status.connects, status.disconnects), (2, 1));
    assert_eq!(
        status.last_error.as_deref(),
        Some("replaced by connection 2")
    );
    loop {
        match read_message(&mut first, "host") {
            Ok(Message::Ping { .. }) => {}
            Err(FrameError::Closed) => break,
            other => panic!("first connection should be closed, got {other:?}"),
        }
    }
}

#[test]
fn send_needs_a_connected_agent_and_a_host_request() {
    let path = socket("send");
    let server = AgentServer::start_with(&path, "unit", fast()).expect("start");
    let error = server.send(&Message::Shutdown).expect_err("nobody there");
    assert_eq!(error.to_string(), "agent is not connected (state waiting)");
    let error = server
        .send(&Message::Ping { seq: 1 })
        .expect_err("not a request");
    assert_eq!(error.to_string(), "ping is not a host request");
}

#[test]
fn stale_sockets_are_replaced_and_live_or_foreign_paths_refused() {
    let path = socket("stale");
    let stale = std::os::unix::net::UnixListener::bind(&path).expect("stale bind");
    drop(stale);
    let server = AgentServer::start_with(&path, "unit", fast()).expect("replaces stale");
    let error = AgentServer::start_with(&path, "unit", fast())
        .err()
        .expect("live socket");
    assert!(matches!(error, AgentServerError::InUse { .. }), "{error}");
    drop(server);
    assert!(!path.exists(), "socket removed on drop");

    std::fs::write(&path, b"not a socket").expect("file");
    let error = AgentServer::start_with(&path, "unit", fast())
        .err()
        .expect("regular file");
    assert!(error.to_string().contains("not a socket"), "{error}");
    std::fs::remove_file(&path).expect("cleanup");
}
