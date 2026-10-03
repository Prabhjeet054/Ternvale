use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_agent::{run, AgentConfig, Exit, SystemActions, Target};
use ternvale_config::{VmConfig, VsockSection};
use ternvale_vmm::VmControl;

use crate::commands::{render, render_agent};
use crate::protocol::{AgentJson, Request, Response};
use crate::server::ControlServer;
use crate::vsock::prepare;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = PathBuf::from(format!("/tmp/tvag-{}-{tag}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("clear leftovers");
        }
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("could not remove {}: {error}", self.0.display());
        }
    }
}

fn config(vsock: Option<VsockSection>) -> VmConfig {
    VmConfig {
        name: "agentvm".into(),
        cpus: 1,
        ram_mib: 256,
        kernel: PathBuf::from("/nonexistent/Image"),
        initrd: None,
        cmdline: String::new(),
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: PathBuf::from("/tmp/serial.log"),
        firmware: None,
        nvram: None,
        firmware_tables: None,
        vsock,
    }
}

fn status(socket: &Path) -> Response {
    crate::client::request(socket, "agentvm", &Request::Status).expect("status")
}

fn agent_of(response: &Response) -> AgentJson {
    response
        .status
        .as_ref()
        .and_then(|status| status.agent.clone())
        .expect("status carries the agent")
}

#[test]
fn status_reports_the_agent_from_waiting_to_connected() {
    let dir = TempDir::new("status");
    let socket = dir.0.join("agentvm.sock");
    let setup = prepare(&config(Some(VsockSection::default())), &socket)
        .expect("prepare")
        .expect("vsock section");
    let uds = dir.0.join("agentvm.vsock");
    assert_eq!(setup.device.uds_dir, uds);
    let mode = std::fs::metadata(&uds)
        .expect("uds dir")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    let host = uds.join("host-5000.sock");
    assert!(host.exists(), "agent server socket");

    let control = Arc::new(VmControl::new("agentvm", 1));
    let _server =
        ControlServer::with_agent(&socket, control, setup.agent.clone()).expect("control");
    let waiting = status(&socket);
    assert_eq!(agent_of(&waiting).state, "waiting");
    let text = render(waiting.status.as_ref().expect("status"), None);
    assert!(
        text.contains("  agent: waiting (no guest agent has connected yet)"),
        "{text}"
    );

    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let agent = std::thread::spawn(move || {
        run(
            &AgentConfig::new(Target::Unix(host)),
            &mut SystemActions::default(),
            &flag,
        )
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let connected = loop {
        let response = status(&socket);
        let json = agent_of(&response);
        if json.state == "connected" && json.pongs >= 1 {
            break response;
        }
        assert!(Instant::now() < deadline, "agent never connected: {json:?}");
        std::thread::sleep(Duration::from_millis(100));
    };
    let line = serde_json::to_string(&connected).expect("encode");
    assert!(
        line.contains(r#""agent":{"state":"connected","version":1,"os":""#),
        "{line}"
    );
    let text = render(connected.status.as_ref().expect("status"), None);
    assert!(text.contains("  agent: connected (v1), "), "{text}");
    assert!(text.contains("pings answered"), "{text}");

    stop.store(true, Ordering::Release);
    assert_eq!(agent.join().expect("agent thread"), Exit::Stopped);
}

#[test]
fn no_section_means_no_vsock_and_long_dirs_are_refused() {
    let dir = TempDir::new("none");
    let socket = dir.0.join("agentvm.sock");
    assert!(prepare(&config(None), &socket).expect("prepare").is_none());
    let long = VsockSection {
        uds_dir: Some(PathBuf::from(format!("/tmp/{}", "d".repeat(90)))),
        ..VsockSection::default()
    };
    let error = prepare(&config(Some(long)), &socket)
        .err()
        .expect("too long");
    assert!(error.to_string().contains("set vsock.uds_dir"), "{error}");
    let off = VsockSection {
        agent: false,
        uds_dir: Some(dir.0.join("v")),
        ..VsockSection::default()
    };
    let setup = prepare(&config(Some(off)), &socket)
        .expect("prepare")
        .expect("section");
    assert!(setup.agent.is_none());
    assert!(!dir.0.join("v/host-5000.sock").exists());
}

#[test]
fn agent_line_explains_disconnects() {
    let agent = AgentJson {
        state: "disconnected".into(),
        version: None,
        os: None,
        agent: None,
        connected_ms: None,
        last_pong_ms: None,
        rtt_us: None,
        connects: 2,
        disconnects: 2,
        pings: 9,
        pongs: 8,
        last_error: Some("no pong within 6000 ms".into()),
    };
    assert_eq!(
        render_agent(&agent),
        "  agent: disconnected; 2 connect(s), 2 disconnect(s); last error: no pong within 6000 ms"
    );
    let connected = AgentJson {
        state: "connected".into(),
        version: Some(1),
        os: Some("Linux 6.6 aarch64".into()),
        agent: Some("ternvale-agent 0.1.0".into()),
        connected_ms: Some(12_345),
        rtt_us: Some(1_234),
        connects: 1,
        disconnects: 0,
        last_error: Some("old".into()),
        ..agent
    };
    assert_eq!(
        render_agent(&connected),
        "  agent: connected (v1), Linux 6.6 aarch64, ternvale-agent 0.1.0, up 12.3s, rtt 1.234 ms, 8/9 pings answered"
    );
    let reconnected = AgentJson {
        connects: 2,
        disconnects: 1,
        rtt_us: None,
        ..connected
    };
    assert_eq!(
        render_agent(&reconnected),
        "  agent: connected (v1), Linux 6.6 aarch64, ternvale-agent 0.1.0, up 12.3s, 8/9 pings answered; 2 connect(s), 1 disconnect(s)"
    );
}
