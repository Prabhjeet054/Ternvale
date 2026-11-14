//! Step 84: `ternvale exec` / `cp` / `wait-ready` with `--json`.
//!
//! The live Unix-agent fixture uses the real guest agent over a Unix socket
//! (same protocol as a VM) plus `ControlServer::with_agent_exec`. The HV test
//! boots `test-assets/virtio-devices` with `allow_exec = true`.

mod support;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use support::Harness;
use ternvale_agent::{run, AgentConfig, Exit, SystemActions, Target};
use ternvale_cli::protocol::Request;
use ternvale_cli::server::ControlServer;
use ternvale_cli::vsock::prepare;
use ternvale_config::{VmConfig, VsockSection};
use ternvale_vmm::VmControl;

const T: &str = "ternvale::cli";
const SIZE: u64 = 500 * 1024 * 1024;
const BOOT: Duration = Duration::from_secs(90);

struct Live {
    harness: Harness,
    _server: ControlServer,
    stop: Arc<AtomicBool>,
    agent: Option<std::thread::JoinHandle<Exit>>,
}

impl Live {
    fn start(tag: &str, allow_exec: bool) -> Self {
        let harness = Harness::new_unsigned(tag);
        let config = dummy_config(&harness.name);
        let setup = prepare(&config, &harness.socket())
            .expect("prepare")
            .expect("vsock");
        let host = setup.device.uds_dir.join("host-5000.sock");
        assert!(host.exists(), "agent server socket");
        let control = Arc::new(VmControl::new(&harness.name, 1));
        let server = ControlServer::with_agent_exec(
            &harness.socket(),
            control,
            setup.agent.clone(),
            allow_exec,
        )
        .expect("control");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let agent = std::thread::spawn(move || {
            run(
                &AgentConfig::new(Target::Unix(host)),
                &mut SystemActions::default(),
                &flag,
            )
        });
        let live = Self {
            harness,
            _server: server,
            stop,
            agent: Some(agent),
        };
        live.wait_connected();
        live
    }

    fn wait_connected(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let out = self.harness.tv(&["--json", "status", &self.harness.name]);
            assert_eq!(out.code, Some(0), "{out:?}");
            assert_jq("status", &out.stdout);
            if out.stdout.contains("\"state\":\"connected\"") {
                tracing::info!(target: T, "agent connected over unix");
                return;
            }
            assert!(Instant::now() < deadline, "agent never connected: {out:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.agent.take() {
            let _ = handle.join();
        }
    }
}

fn dummy_config(name: &str) -> VmConfig {
    VmConfig {
        name: name.into(),
        profile: Default::default(),
        iso: None,
        cpus: 1,
        ram_mib: 256,
        ram_min_mib: None,
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
        vsock: Some(VsockSection::default()),
        tpm: None,
        usb: None,
        audio: None,
        shares: Vec::new(),
        power: Default::default(),
        encryption: false,
        rosetta: false,
        x86_emulation: Default::default(),
        agent: Default::default(),
        backend: Default::default(),
    }
}

fn assert_jq(label: &str, stdout: &str) {
    let text = stdout.trim();
    assert!(!text.is_empty(), "{label}: empty stdout");
    let mut child = Command::new("/usr/bin/jq")
        .args(["-s", "."])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("jq");
    child
        .stdin
        .take()
        .expect("jq stdin")
        .write_all(text.as_bytes())
        .expect("write jq");
    let out = child.wait_with_output().expect("jq wait");
    assert!(
        out.status.success(),
        "{label}: jq rejected {text}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn last_json(stdout: &str) -> serde_json::Value {
    let line = stdout
        .lines()
        .rev()
        .find(|line| line.trim().starts_with('{'))
        .expect("json line");
    serde_json::from_str(line.trim()).expect("parse json")
}

fn sha256_file(path: &Path) -> String {
    let out = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .expect("shasum");
    assert!(out.status.success(), "shasum {path:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("hash")
        .to_string()
}

fn write_blob(path: &Path, size: u64) {
    let mut file = std::fs::File::create(path).expect("create blob");
    file.set_len(size).expect("set_len");
    file.write_all(b"ternvale-step84-copy").expect("header");
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(size.saturating_sub(20)))
        .expect("seek end");
    file.write_all(b"ternvale-step84-tail").expect("tail");
    file.sync_all().expect("sync");
}

#[test]
fn exec_copy_json_timeout_resume_and_jq() {
    let live = Live::start("exec", true);
    let h = &live.harness;
    let name = h.name.clone();
    let guest = h.out.join("guest-blob.bin").display().to_string();
    let _ = std::fs::remove_file(&guest);

    let ready = h.tv(&["--json", "wait-ready", &name, "--timeout", "10"]);
    assert_eq!(ready.code, Some(0), "{ready:?}");
    assert_jq("wait-ready", &ready.stdout);

    let uname = h.tv(&["--json", "exec", &name, "--", "uname", "-a"]);
    assert_eq!(uname.code, Some(0), "{uname:?}");
    assert_jq("exec uname", &uname.stdout);
    assert!(
        uname.stdout.contains("Darwin") || uname.stdout.contains("Linux"),
        "{uname:?}"
    );

    let seven = h.tv(&["--json", "exec", &name, "--", "sh", "-c", "exit 7"]);
    assert_eq!(seven.code, Some(7), "CLI must exit 7: {seven:?}");
    assert_jq("exec exit 7", &seven.stdout);
    assert_eq!(last_json(&seven.stdout)["exit_code"], 7);

    let err = h.tv(&[
        "--json",
        "exec",
        &name,
        "--",
        "sh",
        "-c",
        "echo err-line >&2",
    ]);
    assert_eq!(err.code, Some(0), "{err:?}");
    assert_jq("exec stderr", &err.stdout);
    assert!(
        err.stdout.contains("err-line"),
        "stderr not in json stream: {err:?}"
    );
    assert!(err.stdout.contains("\"stderr\""), "{err:?}");

    let timed = h.tv(&[
        "--json",
        "exec",
        &name,
        "--timeout",
        "1",
        "--",
        "sleep",
        "30",
    ]);
    assert_eq!(timed.code, Some(124), "timeout must exit 124: {timed:?}");
    assert_jq("exec timeout", &timed.stdout);
    assert_eq!(last_json(&timed.stdout)["exit_code"], 124);

    let src = h.out.join("big.bin");
    write_blob(&src, SIZE);
    let src_hash = sha256_file(&src);
    let guest_loc = format!("{name}:{guest}");

    let mut inbound = h.tv_spawn(&["cp", &src.display().to_string(), &guest_loc]);
    std::thread::sleep(Duration::from_millis(80));
    inbound.kill().expect("kill inbound cp");
    let killed = inbound.wait().expect("wait inbound");
    assert!(!killed.success(), "inbound copy finished before interrupt");
    std::thread::sleep(Duration::from_millis(400));
    let partial = std::fs::metadata(&guest).map(|m| m.len()).unwrap_or(0);
    tracing::info!(target: T, partial, "interrupted inbound copy");
    assert!(partial < SIZE, "expected partial guest file, got {partial}");

    let resume_in = h.tv(&["cp", &src.display().to_string(), &guest_loc]);
    assert_eq!(resume_in.code, Some(0), "{resume_in:?}");
    assert_eq!(sha256_file(Path::new(&guest)), src_hash);
    assert_eq!(std::fs::metadata(&guest).expect("guest").len(), SIZE);

    let dst = h.out.join("big.out");
    let mut outbound = h.tv_spawn(&["cp", &guest_loc, &dst.display().to_string()]);
    std::thread::sleep(Duration::from_millis(80));
    outbound.kill().expect("kill outbound cp");
    let killed = outbound.wait().expect("wait outbound");
    assert!(!killed.success(), "outbound copy finished before interrupt");
    std::thread::sleep(Duration::from_millis(400));
    let host_partial = std::fs::metadata(&dst).map(|m| m.len()).unwrap_or(0);
    tracing::info!(target: T, host_partial, "interrupted outbound copy");
    assert!(
        host_partial < SIZE,
        "expected partial host dest, got {host_partial}"
    );

    let resume_out = h.tv(&["cp", &guest_loc, &dst.display().to_string()]);
    assert_eq!(resume_out.code, Some(0), "{resume_out:?}");
    assert_eq!(sha256_file(&dst), src_hash);

    let tiny = h.out.join("tiny.bin");
    std::fs::write(&tiny, b"jq-cp").expect("tiny");
    let tiny_guest = format!("{name}:{}", h.out.join("tiny.guest").display());
    let cp_json = h.tv(&["--json", "cp", &tiny.display().to_string(), &tiny_guest]);
    assert_eq!(cp_json.code, Some(0), "{cp_json:?}");
    assert_jq("cp json", &cp_json.stdout);

    let cfg = h.write_config_with("virtio-devices", 1, "cmdline = \"ternvale.agent=info\"\n");
    let validate = h.tv(&["--json", "validate", &cfg.display().to_string()]);
    assert_eq!(validate.code, Some(0), "{validate:?}");
    assert_jq("validate", &validate.stdout);

    let doctor = h.tv(&["--json", "doctor"]);
    assert_jq("doctor", &doctor.stdout);

    let _ = std::fs::remove_file(&guest);
    tracing::info!(target: T, src_hash, "500 MiB copy checksums matched after resume");
}

#[test]
fn exec_refused_when_allow_exec_false() {
    let live = Live::start("exref", false);
    let h = &live.harness;
    let out = h.tv(&["--json", "exec", &h.name, "--", "uname", "-a"]);
    assert_ne!(out.code, Some(0), "{out:?}");
    assert_jq("exec refused", &out.stdout);
    let msg = format!("{}{}", out.stdout, out.stderr);
    assert!(
        msg.contains("allow_exec"),
        "expected clear allow_exec refusal: {out:?}"
    );
}

#[test]
#[ignore = "needs-hv"]
fn hv_guest_exec_uname_exit_stderr_timeout() {
    let harness = Harness::new("exechv");
    let config = harness.write_config_with(
        "virtio-devices",
        1,
        "cmdline = \"ternvale.agent=info\"\n\n[agent]\nallow_exec = true\n\n[vsock]\ncid = 3\n",
    );
    let mode = std::fs::metadata(&config)
        .ok()
        .map(|m| m.permissions().mode());
    tracing::info!(target: T, config = %config.display(), ?mode, "hv exec config");
    let mut vm = harness.spawn_run(&config);
    vm.wait_serial("the agent connecting", BOOT, |serial| {
        serial.contains("agent connected to host")
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let out = harness.tv(&["--json", "wait-ready", &harness.name, "--timeout", "2"]);
        if out.code == Some(0) {
            assert_jq("hv wait-ready", &out.stdout);
            break;
        }
        assert!(Instant::now() < deadline, "hv wait-ready: {out:?}");
    }

    let uname = harness.tv(&["--json", "exec", &harness.name, "--", "uname", "-a"]);
    assert_eq!(uname.code, Some(0), "{uname:?}");
    assert_jq("hv uname", &uname.stdout);
    assert!(uname.stdout.contains("Linux"), "{uname:?}");

    let seven = harness.tv(&["--json", "exec", &harness.name, "--", "sh", "-c", "exit 7"]);
    assert_eq!(seven.code, Some(7), "{seven:?}");
    assert_jq("hv exit 7", &seven.stdout);

    let err = harness.tv(&[
        "--json",
        "exec",
        &harness.name,
        "--",
        "sh",
        "-c",
        "echo err-line >&2",
    ]);
    assert_eq!(err.code, Some(0), "{err:?}");
    assert_jq("hv stderr", &err.stdout);
    assert!(err.stdout.contains("err-line"), "{err:?}");

    let timed = harness.tv(&[
        "--json",
        "exec",
        &harness.name,
        "--timeout",
        "1",
        "--",
        "sleep",
        "30",
    ]);
    assert_eq!(timed.code, Some(124), "{timed:?}");
    assert_jq("hv timeout", &timed.stdout);

    let stop = harness.tv(&["--json", "stop", &harness.name]);
    assert_jq("hv stop", &stop.stdout);
}

#[test]
fn copy_request_includes_optional_limit() {
    let req = Request::Copy {
        guest_path: "/tmp/a".into(),
        host_path: "/tmp/b".into(),
        to_guest: true,
        offset: None,
        limit: Some(4096),
    };
    let line = serde_json::to_string(&req).expect("enc");
    assert!(line.contains("\"limit\":4096"), "{line}");
    tracing::debug!(target: T, line, "copy request json");
}
