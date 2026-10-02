//! End-to-end guest agent checks through the `ternvale` binary.
//!
//! `status_shows_the_connected_guest_agent` boots the
//! `test-assets/virtio-devices` initramfs with `[vsock]` and `ternvale.agent`
//! on the kernel command line, so the guest init starts the static
//! `ternvale-agent`. `ternvale status` (text and `--json`) must show the agent
//! connected with its OS and answered heartbeats; `ternvale stop` then exits
//! cleanly and removes the agent socket.
//!
//! `rootfs_agent_disconnects_when_killed_and_reconnects_when_restarted` boots
//! a copy of `test-assets/virtio-root/rootfs.ext4` (agent installed by
//! `scripts/install-guest-agent.sh`), waits for `agent: connected (v1)`, kills
//! the agent from the guest console, checks the host logs the disconnect, then
//! restarts it with `ternvale-agent-start` and checks the reconnect.
//!
//! ```sh
//! cargo test -p ternvale-cli --test agent -- --ignored --nocapture
//! ```
//! Artifacts: `target/control-logs/<secs>-agent*/`.

mod support;

use std::time::{Duration, Instant};

use serde_json::Value;
use support::Harness;

const T: &str = "ternvale::cli";
const BOOT: Duration = Duration::from_secs(90);

/// Poll `ternvale status --json` until the agent object satisfies `ready`.
fn wait_agent(harness: &Harness, what: &str, ready: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let out = harness.tv(&["status", &harness.name, "--json"]);
        assert_eq!(out.code, Some(0), "{out:?}");
        let value: Value = serde_json::from_str(out.stdout.trim()).expect("status json");
        let agent = value["status"]["agent"].clone();
        if ready(&agent) {
            tracing::info!(target: T, what, agent = %agent, "agent status reached");
            return agent;
        }
        assert!(Instant::now() < deadline, "no {what}: {agent}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The `  agent: …` line of `ternvale status`.
fn agent_line(harness: &Harness) -> String {
    let out = harness.tv(&["status", &harness.name]);
    assert_eq!(out.code, Some(0), "{out:?}");
    let line = out
        .stdout
        .lines()
        .find(|line| line.starts_with("  agent: "))
        .unwrap_or_else(|| panic!("no agent line in {:?}", out.stdout))
        .to_string();
    tracing::info!(target: T, line = %line, "ternvale status agent line");
    line
}

/// Host log lines (ANSI stripped) that contain `needle`.
fn host_lines(harness: &Harness, needle: &str) -> Vec<String> {
    std::fs::read_to_string(harness.host_log())
        .expect("host log")
        .lines()
        .map(support::strip_ansi)
        .filter(|line| line.contains(needle))
        .collect()
}

#[test]
#[ignore = "needs-hv"]
fn status_shows_the_connected_guest_agent() {
    let harness = Harness::new("agent");
    let name = harness.name.clone();
    let config = harness.write_config_with(
        "virtio-devices",
        1,
        "cmdline = \"ternvale.agent=info\"\n\n[vsock]\ncid = 3\n",
    );
    let validate = harness.tv(&["validate", &config.display().to_string()]);
    let uds = harness.run_dir.join(format!("{name}.vsock"));
    assert!(
        validate.stdout.contains(&format!(
            "  vsock       cid 3, agent on, sockets in {}",
            uds.display()
        )),
        "{validate:?}"
    );

    let mut vm = harness.spawn_run(&config);
    vm.wait_serial("the agent connecting", BOOT, |serial| {
        serial.contains("agent connected to host")
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let text = loop {
        let out = harness.tv(&["status", &name]);
        assert_eq!(out.code, Some(0), "{out:?}");
        if out.stdout.contains("agent: connected") && !out.stdout.contains(" 0/") {
            break out.stdout;
        }
        assert!(Instant::now() < deadline, "agent never answered: {out:?}");
        std::thread::sleep(Duration::from_millis(500));
    };
    let agent_line = text
        .lines()
        .find(|line| line.starts_with("  agent: "))
        .expect("agent line");
    tracing::info!(target: T, line = agent_line, "ternvale status agent line");
    assert!(
        agent_line.starts_with("  agent: connected (v1), Linux 6.6.")
            && agent_line.contains(" aarch64, ternvale-agent 0.1.0, up ")
            && agent_line.contains(", rtt ")
            && agent_line.contains(" pings answered"),
        "{agent_line}"
    );

    let json = harness.tv(&["status", &name, "--json"]);
    let value: serde_json::Value = serde_json::from_str(json.stdout.trim()).expect("json");
    let agent = &value["status"]["agent"];
    tracing::info!(target: T, agent = %agent, "ternvale status --json agent");
    assert_eq!(agent["state"], "connected", "{agent}");
    assert_eq!(agent["version"], 1, "{agent}");
    assert_eq!(agent["connects"], 1, "{agent}");
    assert!(
        agent["os"]
            .as_str()
            .is_some_and(|os| os.starts_with("Linux ")),
        "{agent}"
    );
    assert!(agent["pongs"].as_u64().is_some_and(|n| n >= 1), "{agent}");

    let stop = harness.tv(&["stop", &name]);
    assert_eq!(stop.code, Some(0), "{stop:?}");
    assert_eq!(vm.wait_exit(Duration::from_secs(15)).code(), Some(0));
    assert!(
        !uds.join("host-5000.sock").exists(),
        "agent socket left behind"
    );
    let host = std::fs::read_to_string(harness.host_log()).expect("host log");
    let host: Vec<String> = host.lines().map(support::strip_ansi).collect();
    for want in [
        "agent server listening",
        "agent connected",
        "attached virtio-vsock",
    ] {
        let line = host
            .iter()
            .find(|line| line.contains(want))
            .unwrap_or_else(|| panic!("host log lacks {want:?}"));
        tracing::info!(target: T, "host log: {line}");
    }
}

#[test]
#[ignore = "needs-hv"]
fn rootfs_agent_disconnects_when_killed_and_reconnects_when_restarted() {
    let harness = Harness::new("agentfs");
    let base = support::root().join("test-assets/virtio-root/rootfs.ext4");
    assert!(
        base.is_file(),
        "missing {} (run ./scripts/make-rootfs.sh)",
        base.display()
    );
    let disk = harness.out.join("rootfs.ext4");
    std::fs::copy(&base, &disk).expect("copy rootfs.ext4");
    let config = harness.write_config_with(
        "virtio-root",
        1,
        &format!(
            "boot_disk = true\ncmdline = \"ternvale.agent=info\"\n\n[[disks]]\npath = \"{}\"\nread_only = false\n\n[vsock]\ncid = 3\n",
            disk.display()
        ),
    );
    let mut vm = harness.spawn_run(&config);
    vm.wait_serial("the agent starting from the rootfs", BOOT, |serial| {
        serial.contains("ternvale root ready") && serial.contains("ternvale agent started pid=")
    });

    let first = wait_agent(&harness, "the first connect", |agent| {
        agent["state"] == "connected" && !agent["last_pong_ms"].is_null()
    });
    assert_eq!(first["version"], 1, "{first}");
    assert_eq!(first["connects"], 1, "{first}");
    let line = agent_line(&harness);
    assert!(
        line.starts_with("  agent: connected (v1), Linux 6.6.")
            && line.contains(" aarch64, ternvale-agent 0.1.0, up ")
            && !line.contains(" connect(s)"),
        "{line}"
    );

    let killed = Instant::now();
    vm.type_chunks(&["kill $(pidof ", "ternvale-agent)\n"]);
    let gone = wait_agent(&harness, "the disconnect", |agent| {
        agent["state"] == "disconnected"
    });
    let disconnect_ms = killed.elapsed().as_millis();
    assert_eq!(gone["connects"], 1, "{gone}");
    assert_eq!(gone["disconnects"], 1, "{gone}");
    assert_eq!(gone["last_error"], "agent closed the connection", "{gone}");
    let line = agent_line(&harness);
    assert_eq!(
        line,
        "  agent: disconnected; 1 connect(s), 1 disconnect(s); last error: agent closed the connection"
    );
    let disconnects = host_lines(&harness, "agent disconnected");
    assert_eq!(disconnects.len(), 1, "{disconnects:#?}");
    assert!(
        disconnects[0].contains("reason=agent closed the connection"),
        "{}",
        disconnects[0]
    );
    tracing::info!(target: T, disconnect_ms, line = %disconnects[0], "host logged the disconnect");
    // `$((6*7))` keeps the marker out of the echoed command line.
    vm.type_chunks(&[
        "sleep 1; pidof ",
        "ternvale-agent ",
        "|| echo gone-",
        "$((6*7))\n",
    ]);
    vm.wait_serial(
        "the agent process gone",
        Duration::from_secs(10),
        |serial| serial.contains("gone-42"),
    );

    let restarted = Instant::now();
    vm.type_chunks(&["/usr/sbin/", "ternvale-agent-", "start\n"]);
    let back = wait_agent(&harness, "the reconnect", |agent| {
        agent["state"] == "connected" && agent["connects"] == 2 && !agent["last_pong_ms"].is_null()
    });
    let reconnect_ms = restarted.elapsed().as_millis();
    assert_eq!(back["disconnects"], 1, "{back}");
    let line = agent_line(&harness);
    assert!(
        line.starts_with("  agent: connected (v1), Linux 6.6.")
            && line.ends_with("; 2 connect(s), 1 disconnect(s)"),
        "{line}"
    );
    let serial = vm.serial();
    assert_eq!(
        serial.matches("agent connected to host").count(),
        2,
        "guest console should show two handshakes"
    );
    let connects = host_lines(&harness, "agent connected");
    assert_eq!(connects.len(), 2, "{connects:#?}");
    tracing::info!(target: T, reconnect_ms, line = %connects[1], "host logged the reconnect");

    let stop = harness.tv(&["stop", &harness.name]);
    assert_eq!(stop.code, Some(0), "{stop:?}");
    assert_eq!(vm.wait_exit(Duration::from_secs(15)).code(), Some(0));
}
