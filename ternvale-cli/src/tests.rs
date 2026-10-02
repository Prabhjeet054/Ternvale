use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::{CommandFactory, Parser};
use ternvale_vmm::{VmControl, VmState};

use crate::cli::{Cli, Command};
use crate::client::{self, Reach};
use crate::protocol::{Request, Response, MAX_LINE};
use crate::server::ControlServer;

/// Held by tests that spawn a child process and by tests that need a dropped
/// socket to be closed. macOS marks a new socket close-on-exec only after
/// creating it, so a child spawned in between keeps the listener alive.
pub(crate) static CHILD_PROCESS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A short per-test directory under /tmp; `sun_path` is only 104 bytes.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(format!("/tmp/tvcli-{}-{n}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("clear leftovers");
        }
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn socket(&self) -> PathBuf {
        self.0.join("vm.sock")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("could not remove {}: {error}", self.0.display());
        }
    }
}

fn parse(args: &[&str]) -> Command {
    Cli::try_parse_from(args).expect("parse").command
}

#[test]
fn clap_grammar_is_consistent() {
    Cli::command().debug_assert();
}

#[test]
fn parses_every_subcommand() {
    assert_eq!(
        parse(&["ternvale", "run", "vm.toml"]),
        Command::Run {
            config: "vm.toml".into()
        }
    );
    assert_eq!(
        parse(&["ternvale", "validate", "vm.toml"]),
        Command::Validate {
            config: "vm.toml".into()
        }
    );
    assert_eq!(
        parse(&["ternvale", "create-disk", "d.img", "4G"]),
        Command::CreateDisk {
            path: "d.img".into(),
            size: "4G".into()
        }
    );
    assert_eq!(
        parse(&["ternvale", "status", "a", "--stats"]),
        Command::Status {
            name: "a".into(),
            stats: true,
            json: false
        }
    );
    assert_eq!(
        parse(&["ternvale", "pause", "a", "--timeout-ms", "250", "--json"]),
        Command::Pause {
            name: "a".into(),
            timeout_ms: 250,
            json: true
        }
    );
    assert_eq!(
        parse(&["ternvale", "resume", "a"]),
        Command::Resume {
            name: "a".into(),
            json: false
        }
    );
    assert_eq!(
        parse(&["ternvale", "stop", "a", "--force", "--no-wait"]),
        Command::Stop {
            name: "a".into(),
            force: true,
            no_wait: true,
            json: false
        }
    );
    assert!(Cli::try_parse_from(["ternvale", "stop"]).is_err());
    assert!(Cli::try_parse_from(["ternvale", "reboot", "a"]).is_err());
}

#[test]
fn error_chain_drops_causes_already_in_the_message() {
    let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
    let error = anyhow::Error::new(io)
        .context("read config x: gone")
        .context("validate x");
    assert_eq!(
        crate::error_chain(&error),
        "validate x: read config x: gone"
    );
    let plain = anyhow::anyhow!("inner").context("outer");
    assert_eq!(crate::error_chain(&plain), "outer: inner");
}

fn ask(path: &std::path::Path, request: Request) -> Response {
    client::request(path, "vm", &request).expect("request")
}

fn state(response: &Response) -> String {
    response.status.as_ref().expect("status").state.clone()
}

#[test]
fn server_drives_the_state_machine_over_the_socket() {
    let dir = TempDir::new();
    let path = dir.socket();
    let control = Arc::new(VmControl::new("vm", 1));
    let server = ControlServer::start(&path, Arc::clone(&control)).expect("start");
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "socket mode {mode:o}");

    assert_eq!(state(&ask(&path, Request::Status)), "created");
    let refused = ask(
        &path,
        Request::Pause {
            timeout_ms: Some(100),
        },
    );
    assert!(!refused.ok);
    assert!(
        refused.error.as_deref().unwrap_or("").contains("created"),
        "{refused:?}"
    );

    control.mark_running();
    let paused = ask(
        &path,
        Request::Pause {
            timeout_ms: Some(1_000),
        },
    );
    assert_eq!(state(&paused), "paused", "{paused:?}");
    assert_eq!(control.state(), VmState::Paused);
    let resumed = ask(&path, Request::Resume);
    assert_eq!(state(&resumed), "running");
    assert_eq!(resumed.status.as_ref().map(|s| s.pauses), Some(1));

    let stats = ask(&path, Request::QueryStats);
    assert_eq!(
        stats.stats.as_ref().map(|s| s.cpus.len()),
        Some(1),
        "{stats:?}"
    );

    let stopping = ask(&path, Request::Shutdown);
    assert_eq!(state(&stopping), "stopping");
    assert_eq!(
        stopping
            .status
            .as_ref()
            .and_then(|s| s.stop_cause.clone())
            .as_deref(),
        Some("shutdown")
    );
    let forced = ask(&path, Request::ForceStop);
    assert_eq!(
        forced
            .status
            .as_ref()
            .and_then(|s| s.stop_cause.clone())
            .as_deref(),
        Some("force-stop")
    );
    assert!(control.forced_for().is_some());
    let paused_while_stopping = ask(&path, Request::Pause { timeout_ms: None });
    assert_eq!(
        paused_while_stopping.error.as_deref(),
        Some("cannot pause a vm that is stopping; pause needs a vm that is running (or already paused)")
    );

    control.finish(Ok(ternvale_vmm::ExitReason::Canceled));
    assert_eq!(state(&ask(&path, Request::Status)), "stopped");
    for (request, verb) in [
        (Request::Pause { timeout_ms: None }, "pause"),
        (Request::Resume, "resume"),
        (Request::Shutdown, "shut down"),
    ] {
        let refused = ask(&path, request);
        assert!(!refused.ok, "{refused:?}");
        let want = format!("cannot {verb} a vm that is stopped;");
        assert!(
            refused.error.as_deref().unwrap_or("").starts_with(&want),
            "{refused:?}"
        );
        assert!(refused.status.is_none());
    }

    drop(server);
    assert!(!path.exists(), "socket removed on drop");
    assert_eq!(
        client::connect(&path).expect("connect").err(),
        Some(Reach::NotRunning)
    );
    let gone = client::request(&path, "vm", &Request::Pause { timeout_ms: None })
        .expect_err("pause after exit");
    assert_eq!(
        format!("{gone:#}"),
        format!(
            "cannot pause vm vm: it is not running (no control socket at {})",
            path.display()
        )
    );
}

#[test]
fn bad_lines_get_an_error_response_and_long_lines_close_the_connection() {
    let dir = TempDir::new();
    let path = dir.socket();
    let control = Arc::new(VmControl::new("vm", 1));
    let _server = ControlServer::start(&path, control).expect("start");

    let stream = UnixStream::connect(&path).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);
    let mut roundtrip = |line: &[u8]| -> std::io::Result<String> {
        writer.write_all(line)?;
        let mut reply = String::new();
        reader.read_line(&mut reply)?;
        Ok(reply)
    };
    let bad: Response =
        serde_json::from_str(&roundtrip(b"status\n").expect("bad line")).expect("json");
    assert!(!bad.ok);
    assert!(
        bad.error
            .as_deref()
            .unwrap_or("")
            .starts_with("malformed request"),
        "{bad:?}"
    );
    let status = b"{\"cmd\":\"status\"}\n";
    let good: Response = serde_json::from_str(&roundtrip(status).expect("status")).expect("json");
    assert!(good.ok, "the connection survives a malformed line");

    let mut long = vec![b'x'; MAX_LINE + 10];
    long.push(b'\n');
    let reply: Response = serde_json::from_str(&roundtrip(&long).expect("long")).expect("json");
    assert!(!reply.ok);
    assert!(
        reply.error.as_deref().unwrap_or("").contains("longer than"),
        "{reply:?}"
    );
    match roundtrip(status) {
        Ok(after) => assert!(after.is_empty(), "server still answering: {after}"),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe, "{error}"),
    }
}

#[test]
fn stale_sockets_are_replaced_and_live_ones_are_not() {
    let _no_children = CHILD_PROCESS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = TempDir::new();
    let path = dir.socket();
    drop(UnixListener::bind(&path).expect("bind"));
    assert!(path.exists(), "a dropped listener leaves its socket file");
    assert_eq!(
        client::connect(&path).expect("connect").err(),
        Some(Reach::Stale)
    );
    let error = client::request(&path, "vm", &Request::Status).expect_err("stale");
    assert!(
        format!("{error:#}").contains("stale control socket"),
        "{error:#}"
    );

    let server =
        ControlServer::start(&path, Arc::new(VmControl::new("vm", 1))).expect("replace stale");
    let duplicate = ControlServer::start(&path, Arc::new(VmControl::new("vm", 1)));
    let error = duplicate.err().expect("second server on a live socket");
    assert!(
        format!("{error:#}").contains("already serving"),
        "{error:#}"
    );
    assert!(
        path.exists(),
        "the failed start must not remove the live socket"
    );
    assert!(ask(&path, Request::Status).ok);
    drop(server);
}

#[test]
fn wait_gone_returns_once_the_socket_disappears() {
    let dir = TempDir::new();
    let path = dir.socket();
    let server = ControlServer::start(&path, Arc::new(VmControl::new("vm", 1))).expect("start");
    let error =
        crate::commands::wait_gone(&path, Duration::from_millis(150)).expect_err("still up");
    assert!(format!("{error:#}").contains("still serving"), "{error:#}");
    let dropper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        drop(server);
    });
    crate::commands::wait_gone(&path, Duration::from_secs(5)).expect("gone");
    dropper.join().expect("dropper");
}
