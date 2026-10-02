use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ternvale_agent_proto::{read_message, write_message, ClipboardText, Message, PROTOCOL_VERSION};

use super::*;

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<String>>>);

impl Recorder {
    fn calls(&self) -> Vec<String> {
        self.0.lock().expect("recorder").clone()
    }
}

impl Actions for Recorder {
    fn set_resolution(&mut self, width: u32, height: u32) -> Result<(), AgentError> {
        self.0
            .lock()
            .expect("recorder")
            .push(format!("resolution {width}x{height}"));
        Ok(())
    }
    fn clipboard_set(&mut self, text: &str) -> Result<(), AgentError> {
        self.0
            .lock()
            .expect("recorder")
            .push(format!("clipboard {text}"));
        Ok(())
    }
    fn shutdown(&mut self) -> Result<(), AgentError> {
        self.0.lock().expect("recorder").push("shutdown".into());
        Ok(())
    }
}

fn socket(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("tva-{}-{name}.sock", std::process::id()));
    if let Err(error) = std::fs::remove_file(&path) {
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error}");
    }
    path
}

fn fast(path: &std::path::Path) -> AgentConfig {
    AgentConfig {
        backoff: Backoff::new(Duration::from_millis(20), Duration::from_millis(160)),
        handshake_timeout: Duration::from_millis(500),
        idle_timeout: Duration::from_secs(2),
        poll: Duration::from_millis(20),
        ..AgentConfig::new(Target::Unix(path.to_path_buf()))
    }
}

struct Running {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Exit>,
    recorder: Recorder,
}

fn start(config: AgentConfig) -> Running {
    let stop = Arc::new(AtomicBool::new(false));
    let recorder = Recorder::default();
    let (flag, mut actions) = (Arc::clone(&stop), recorder.clone());
    let thread = std::thread::spawn(move || run(&config, &mut actions, &flag));
    Running {
        stop,
        thread,
        recorder,
    }
}

impl Running {
    fn stop(self) -> Exit {
        self.stop.store(true, Ordering::Release);
        self.thread.join().expect("agent thread")
    }
}

/// Accept one connection, check `Hello`, and reply `Welcome`.
fn accept_and_welcome(listener: &UnixListener) -> UnixStream {
    let (mut stream, _) = listener.accept().expect("accept");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    match read_message(&mut stream, "agent").expect("hello") {
        Message::Hello { version, os, agent } => {
            assert_eq!(version, PROTOCOL_VERSION);
            assert!(os.contains(std::env::consts::ARCH), "{os}");
            assert!(agent.starts_with("ternvale-agent "), "{agent}");
        }
        other => panic!("expected hello, got {other:?}"),
    }
    write_message(&mut stream, &Message::Welcome { version: 1 }, "agent").expect("welcome");
    stream
}

#[test]
fn handshake_then_answers_ping_and_carries_out_requests() {
    let path = socket("serve");
    let listener = UnixListener::bind(&path).expect("bind");
    let agent = start(fast(&path));
    let mut host = accept_and_welcome(&listener);
    for seq in [1, u64::MAX] {
        write_message(&mut host, &Message::Ping { seq }, "agent").expect("ping");
        assert_eq!(
            read_message(&mut host, "agent").expect("pong"),
            Message::Pong { seq }
        );
    }
    let unknown = br#"{"type":"from_the_future","x":1}"#;
    let mut frame = (unknown.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(unknown);
    std::io::Write::write_all(&mut host, &frame).expect("unknown frame");
    for message in [
        Message::SetResolution {
            width: 1280,
            height: 800,
        },
        Message::ClipboardSet {
            text: ClipboardText("copied".into()),
        },
        Message::Ping { seq: 3 },
    ] {
        write_message(&mut host, &message, "agent").expect("send");
    }
    assert_eq!(
        read_message(&mut host, "agent").expect("pong after unknown"),
        Message::Pong { seq: 3 }
    );
    write_message(&mut host, &Message::Shutdown, "agent").expect("shutdown");
    assert_eq!(agent.thread.join().expect("agent"), Exit::Shutdown);
    assert_eq!(
        agent.recorder.calls(),
        ["resolution 1280x800", "clipboard copied", "shutdown"]
    );
    std::fs::remove_file(&path).expect("cleanup");
}

#[test]
fn retries_with_backoff_until_the_host_appears_and_after_it_goes_away() {
    let path = socket("backoff");
    let agent = start(fast(&path));
    std::thread::sleep(Duration::from_millis(400));
    let listener = UnixListener::bind(&path).expect("bind");
    let host = accept_and_welcome(&listener);
    drop(host);
    let gone = Instant::now();
    let host = accept_and_welcome(&listener);
    let reconnect = gone.elapsed();
    assert!(
        reconnect < Duration::from_millis(500),
        "backoff reset after handshake, reconnect took {reconnect:?}"
    );
    drop(host);
    drop(listener);
    std::fs::remove_file(&path).expect("unlink");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(agent.stop(), Exit::Stopped);
}

#[test]
fn reject_and_handshake_timeout_lead_to_retries() {
    let path = socket("reject");
    let listener = UnixListener::bind(&path).expect("bind");
    let agent = start(fast(&path));
    let (mut stream, _) = listener.accept().expect("accept");
    assert!(matches!(
        read_message(&mut stream, "agent"),
        Ok(Message::Hello { .. })
    ));
    let reject = Message::Reject {
        reason: "too new".into(),
        min_version: 9,
        max_version: 9,
    };
    write_message(&mut stream, &reject, "agent").expect("reject");
    let (mut silent, _) = listener.accept().expect("retry after reject");
    assert!(matches!(
        read_message(&mut silent, "agent"),
        Ok(Message::Hello { .. })
    ));
    let waited = Instant::now();
    let (_third, _) = listener.accept().expect("retry after handshake timeout");
    assert!(
        waited.elapsed() >= Duration::from_millis(500),
        "{:?}",
        waited.elapsed()
    );
    assert_eq!(agent.stop(), Exit::Stopped);
    std::fs::remove_file(&path).expect("cleanup");
}

#[test]
fn silent_host_triggers_reconnect_after_idle_timeout() {
    let path = socket("idle");
    let listener = UnixListener::bind(&path).expect("bind");
    let config = AgentConfig {
        idle_timeout: Duration::from_millis(300),
        ..fast(&path)
    };
    let agent = start(config);
    let _first = accept_and_welcome(&listener);
    let welcomed = Instant::now();
    let _second = accept_and_welcome(&listener);
    let elapsed = welcomed.elapsed();
    assert!(
        elapsed >= Duration::from_millis(300) && elapsed < Duration::from_secs(2),
        "{elapsed:?}"
    );
    assert_eq!(agent.stop(), Exit::Stopped);
    std::fs::remove_file(&path).expect("cleanup");
}

#[test]
fn backoff_doubles_to_the_cap_and_resets() {
    let mut backoff = Backoff::new(Duration::from_millis(200), Duration::from_secs(5));
    let delays: Vec<u64> = (0..8)
        .map(|_| backoff.next_delay().as_millis() as u64)
        .collect();
    assert_eq!(delays, [200, 400, 800, 1600, 3200, 5000, 5000, 5000]);
    backoff.reset();
    assert_eq!(backoff.next_delay(), Duration::from_millis(200));
    let mut tiny = Backoff::new(Duration::from_secs(9), Duration::from_secs(1));
    assert_eq!(tiny.next_delay(), Duration::from_secs(1));
}

#[test]
fn vsock_target_is_unsupported_off_linux_and_targets_display() {
    let target = Target::Vsock { cid: 2, port: 5000 };
    assert_eq!(target.to_string(), "vsock:2:5000");
    assert_eq!(
        Target::Unix("/tmp/x.sock".into()).to_string(),
        "unix:/tmp/x.sock"
    );
    if !cfg!(target_os = "linux") {
        assert!(matches!(
            transport::connect(&target),
            Err(AgentError::Unsupported("vsock"))
        ));
    }
    let os = os_description();
    assert!(os.ends_with(std::env::consts::ARCH), "{os}");
}
