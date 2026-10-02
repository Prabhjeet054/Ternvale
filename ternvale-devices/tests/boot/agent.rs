//! Guest agent scenario: the static `ternvale-agent` in the virtio-devices
//! initramfs connects over vsock to the host `AgentServer`, answers
//! heartbeats, reconnects after the host server restarts, carries out
//! `ClipboardSet` / `SetResolution`, and powers the guest off on `Shutdown`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_agent_proto::{ClipboardText, Message, AGENT_PORT};
use ternvale_config::VmConfig;
use ternvale_devices::{
    host_socket_path, last_lines, AgentServer, AgentState, AgentStatus, Pl011, Step, VirtioVsock,
    VsockConfig,
};
use ternvale_vmm::{ExitReason, Machine, MachineError};

use crate::common::{
    assets_root, banner_timeout, boot_cpus, drive, init_logging_at, log_dir, restore_stdin,
    stdin_pipe, write_result,
};

const GUEST_CID: u32 = 3;
const HOST_DOWN: Duration = Duration::from_millis(1500);
const RECONNECT: Duration = Duration::from_secs(15);
const LOG_FILTER: &str = "info,ternvale::agent=debug,ternvale::virtio::vsock=debug";

pub fn run() -> Result<(), String> {
    let root = assets_root().join("virtio-devices");
    for name in ["Image", "initramfs.cpio"] {
        let path = root.join(name);
        if !path.is_file() {
            return Err(format!(
                "missing {} (run ./scripts/make-devices-initramfs.sh)",
                path.display()
            ));
        }
    }
    let log_dir = log_dir();
    let guard = init_logging_at("boot-agent", &log_dir, LOG_FILTER)?;
    let uds_dir = std::env::temp_dir().join(format!("tva-{}", std::process::id()));
    std::fs::create_dir_all(&uds_dir).map_err(|err| format!("create uds dir: {err}"))?;
    let agent_sock = host_socket_path(&uds_dir, AGENT_PORT);
    let server = AgentServer::start(&agent_sock, "boot-agent").map_err(|err| err.to_string())?;
    tracing::info!(target: "ternvale::boot", dir = %log_dir.display(), host_log = %guard.log_path().display(), uds = %agent_sock.display(), scenario = "agent", "boot harness logs");

    let serial_log = log_dir.join("guest-serial.log");
    let vm = VmConfig {
        name: "boot-agent".to_string(),
        cpus: boot_cpus(1)?,
        ram_mib: 256,
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio")),
        cmdline: "ternvale.agent=debug".to_string(),
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: serial_log.clone(),
        firmware: None,
        nvram: None,
        vsock: None,
    };
    let uart = Pl011::open(&serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let host = guard.log_path().to_path_buf();
    let feeder =
        std::thread::spawn(move || drive(&serial_for_feed, &mut input, &flag, &host, script()));
    let sock = agent_sock.clone();
    let controller = std::thread::spawn(move || control(server, &sock));

    let vsock_config = VsockConfig {
        guest_cid: Some(GUEST_CID),
        uds_dir: uds_dir.clone(),
        listen_ports: Vec::new(),
    };
    let started = Instant::now();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let (base, size, vsock, _stats, cid) = VirtioVsock::attach(
            0,
            &vsock_config,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(0),
        )
        .map_err(|error| MachineError::Attach(error.to_string()))?;
        tracing::info!(target: "ternvale::boot", guest_cid = cid, "attached virtio-vsock");
        Ok(vec![(base, size, vsock)])
    });
    cancel.store(true, Ordering::Release);
    let script_result = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let control_result = controller
        .join()
        .unwrap_or_else(|_| Err("controller panicked".to_string()));
    cleanup(&uds_dir);

    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(&log_dir, &exit, &script_result, &serial);
    let fail = |why: String| {
        format!(
            "{why}\nexit={exit:?}\nlog={}\n--- serial ---\n{}",
            guard.log_path().display(),
            last_lines(&serial, 40)
        )
    };
    script_result.map_err(fail)?;
    let summary = control_result.map_err(fail)?;
    check_guest_log(&serial).map_err(fail)?;
    if !matches!(exit, Ok(ExitReason::SystemOff)) {
        return Err(fail("guest did not power off after Shutdown".into()));
    }
    tracing::info!(
        target: "ternvale::boot",
        total_ms = started.elapsed().as_millis() as u64,
        connect_ms = summary.connect_ms,
        reconnect_ms = summary.reconnect_ms,
        os = ?summary.first.os,
        agent = ?summary.first.agent,
        version = ?summary.first.version,
        pongs = summary.first.pongs,
        rtt_us = ?summary.first.rtt_us,
        scenario = "agent",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

struct Summary {
    first: AgentStatus,
    connect_ms: u64,
    reconnect_ms: u64,
}

/// Host side: wait for the agent, restart the server, send requests, shut down.
fn control(server: AgentServer, sock: &Path) -> Result<Summary, String> {
    let boot = Instant::now();
    let first = server
        .wait_for(banner_timeout() + Duration::from_secs(30), |s| {
            s.state == AgentState::Connected && s.pongs >= 2
        })
        .ok_or_else(|| {
            format!(
                "agent never connected and answered 2 pings: {:?}",
                server.status()
            )
        })?;
    let connect_ms = boot.elapsed().as_millis() as u64;
    tracing::info!(target: "ternvale::boot", status = ?first, "agent connected and answering heartbeats");
    let os = first.os.clone().unwrap_or_default();
    if !os.starts_with("Linux ") || !os.ends_with(" aarch64") {
        return Err(format!("unexpected agent os {os:?}"));
    }
    if first.version != Some(1) {
        return Err(format!("unexpected protocol version {:?}", first.version));
    }

    drop(server);
    tracing::info!(target: "ternvale::boot", down_ms = HOST_DOWN.as_millis() as u64, "agent server stopped; agent should retry with backoff");
    std::thread::sleep(HOST_DOWN);
    let server = AgentServer::start(sock, "boot-agent").map_err(|err| err.to_string())?;
    let restarted = Instant::now();
    let second = server
        .wait_for(RECONNECT, |s| s.state == AgentState::Connected)
        .ok_or_else(|| format!("agent did not reconnect: {:?}", server.status()))?;
    let reconnect_ms = restarted.elapsed().as_millis() as u64;
    tracing::info!(target: "ternvale::boot", reconnect_ms, status = ?second, "agent reconnected after the host restart");

    let requests = [
        Message::ClipboardSet {
            text: ClipboardText("hello from the host".into()),
        },
        Message::SetResolution {
            width: 1280,
            height: 800,
        },
    ];
    for request in &requests {
        server.send(request).map_err(|err| err.to_string())?;
    }
    server
        .wait_for(Duration::from_secs(5), |s| s.pongs >= 1)
        .ok_or("no heartbeat after requests")?;
    server
        .send(&Message::Shutdown)
        .map_err(|err| err.to_string())?;
    server
        .wait_for(Duration::from_secs(20), |s| {
            s.state == AgentState::Disconnected
        })
        .ok_or("agent connection did not end after Shutdown")?;
    Ok(Summary {
        first,
        connect_ms,
        reconnect_ms,
    })
}

/// The agent's own console log must show each stage.
fn check_guest_log(serial: &str) -> Result<(), String> {
    let wanted = [
        "agent connected to host",
        "agent message direction=\"recv\" peer=\"host\" kind=\"ping\"",
        "agent not connected; retrying",
        "clipboard updated len=19",
        "no display in this guest; resolution not applied width=1280 height=800",
        "host requested shutdown; syncing and powering off",
    ];
    let mut from = 0;
    for want in wanted {
        match serial[from..].find(want) {
            Some(at) => from += at + want.len(),
            None => return Err(format!("guest log lacks {want:?} (in order)")),
        }
    }
    let connects = serial.matches("agent connected to host").count();
    if connects < 2 {
        return Err(format!("expected a reconnect, saw {connects} connect(s)"));
    }
    Ok(())
}

fn cleanup(dir: &Path) {
    if let Err(error) = std::fs::remove_dir_all(dir) {
        tracing::warn!(target: "ternvale::boot", dir = %dir.display(), error = %error, "could not remove uds dir");
    }
}

fn expect(pattern: &str, timeout: Duration) -> Step {
    Step::Expect {
        pattern: pattern.to_string(),
        timeout,
    }
}

fn script() -> Vec<Step> {
    let banner = banner_timeout();
    vec![
        expect("Linux version", banner),
        expect("ternvale devices ready", banner),
        expect("vsock=yes", banner),
        expect("ternvale agent started", banner),
        expect("agent connected to host", banner),
    ]
}
