//! Step 77 live: simulated host sleep (SIGUSR1/2, not `pmset sleepnow`),
//! Linux `stop --graceful` with the guest agent, and force-stop when the
//! guest ignores KEY_POWER.
//!
//! ```sh
//! TERNVALE_POWER_SLEEP_SECS=120 cargo test -p ternvale-cli --test power \
//!   -- --ignored --nocapture --test-threads=1
//! ```
//! Artifacts: `target/control-logs/<secs>-pwr*/`.

mod support;

use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use support::Harness;

const T: &str = "ternvale::cli";
const BOOT: Duration = Duration::from_secs(90);

fn sleep_secs() -> u64 {
    std::env::var("TERNVALE_POWER_SLEEP_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(120)
}

fn date_lines(serial: &str) -> Vec<String> {
    serial
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words.len() >= 6
                && words[words.len() - 2] == "UTC"
                && words[words.len() - 1].len() == 4
                && words[words.len() - 1].chars().all(|c| c.is_ascii_digit())
        })
        .collect()
}

fn host_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}

/// macOS `date -j` parse of busybox `Sat Nov  7 16:34:01 UTC 2026`.
fn unix_from_date_line(line: &str) -> i64 {
    for fmt in ["%a %b %e %H:%M:%S UTC %Y", "%a %b %d %H:%M:%S UTC %Y"] {
        let out = Command::new("date")
            .args(["-u", "-j", "-f", fmt, line, "+%s"])
            .output()
            .expect("date -j");
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Ok(n) = text.trim().parse() {
                return n;
            }
        }
    }
    panic!("could not parse date line {line:?}");
}

fn host_lines(harness: &Harness, needle: &str) -> Vec<String> {
    std::fs::read_to_string(harness.host_log())
        .expect("host log")
        .lines()
        .map(support::strip_ansi)
        .filter(|line| line.contains(needle))
        .collect()
}

fn wait_host(harness: &Harness, needle: &str, limit: Duration) {
    let deadline = Instant::now() + limit;
    loop {
        if !host_lines(harness, needle).is_empty() {
            tracing::info!(target: T, needle, "host log matched");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no {needle:?} in {} after {limit:?}",
            harness.host_log().display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn json_state(harness: &Harness) -> String {
    let out = harness.tv(&["status", &harness.name, "--json"]);
    assert_eq!(out.code, Some(0), "{out:?}");
    let value: serde_json::Value = serde_json::from_str(out.stdout.trim()).expect("status json");
    value["status"]["state"].as_str().unwrap_or("").to_string()
}

fn wait_state(harness: &Harness, want: &str, limit: Duration) {
    let deadline = Instant::now() + limit;
    loop {
        let state = json_state(harness);
        if state == want {
            tracing::info!(target: T, state = %state, "vm reached state");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "state {state} never became {want}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn kill_signal(pid: u32, sig: &str) {
    let status = Command::new("kill")
        .args([format!("-{sig}"), pid.to_string()])
        .status()
        .expect("kill");
    assert!(status.success(), "kill -{sig} {pid} failed: {status}");
    tracing::info!(target: T, pid, sig, "sent host-power injection signal");
}

fn start_date_loop(vm: &mut support::Vm) {
    vm.type_chunks(&[
        "while true; do ",
        "/bin/busybox ",
        "date; ",
        "/bin/busybox ",
        "sleep 1; done\n",
    ]);
    vm.wait_serial("three date lines", Duration::from_secs(20), |s| {
        date_lines(s).len() >= 3
    });
}

#[test]
#[ignore = "needs-hv"]
fn linux_sleep_wake_date_resync_under_a_few_seconds() {
    let harness = Harness::new("pwr-sleep");
    let name = harness.name.clone();
    let paused_for = Duration::from_secs(sleep_secs());
    let config = harness.write_config_with(
        "virtio-devices",
        1,
        "cmdline = \"ternvale.agent=info\"\n\n[vsock]\ncid = 3\nserial = false\n",
    );
    let mut vm = harness.spawn_run(&config);
    vm.wait_serial("the agent connecting", BOOT, |serial| {
        serial.contains("agent connected to host") && serial.contains("# ")
    });
    wait_state(&harness, "running", Duration::from_secs(20));
    start_date_loop(&mut vm);

    let before_sleep = date_lines(&vm.serial());
    let last_before = before_sleep.last().expect("date before sleep").clone();
    let host_at_sleep = host_unix();
    tracing::info!(
        target: T,
        last_before = %last_before,
        host_unix = host_at_sleep,
        pause_s = paused_for.as_secs(),
        "injecting WillSleep via SIGUSR1 (Mac is not put to sleep)"
    );
    kill_signal(vm.pid(), "USR1");
    wait_host(
        &harness,
        "host sleep: pausing vcpus",
        Duration::from_secs(5),
    );
    wait_state(&harness, "paused", Duration::from_secs(10));
    std::thread::sleep(paused_for);

    kill_signal(vm.pid(), "USR2");
    wait_host(
        &harness,
        "host wake: resuming vcpus",
        Duration::from_secs(5),
    );
    wait_state(&harness, "running", Duration::from_secs(10));
    wait_host(&harness, "sent agent time_sync", Duration::from_secs(15));

    let count = date_lines(&vm.serial()).len();
    vm.wait_serial("date after wake", Duration::from_secs(10), |s| {
        date_lines(s).len() > count
    });
    std::thread::sleep(Duration::from_secs(2));
    let after = date_lines(&vm.serial());
    let last_after = after.last().expect("date after wake").clone();
    let guest = unix_from_date_line(&last_after);
    let host = host_unix();
    let delta = guest - host;
    let note = format!(
        "guest_date={last_after}\nguest_unix={guest}\nhost_unix={host}\ndelta_s={delta}\npause_s={}\nmac_sleep=false (SIGUSR1/2)\n",
        paused_for.as_secs()
    );
    std::fs::write(harness.out.join("date-delta.txt"), &note).expect("date-delta");
    tracing::info!(
        target: T,
        last_after = %last_after,
        guest_unix = guest,
        host_unix = host,
        delta_s = delta,
        "guest vs host date after agent time_sync"
    );
    assert!(
        delta.abs() <= 5,
        "guest/host date delta {delta}s after {pause}s simulated sleep (want |delta| ≤ 5); {note}",
        pause = paused_for.as_secs()
    );

    let stop = harness.tv(&["stop", &name, "--force"]);
    assert_eq!(stop.code, Some(0), "{stop:?}");
    let _ = vm.wait_exit(Duration::from_secs(15));
}

#[test]
#[ignore = "needs-hv"]
fn linux_graceful_stop_clean_os_shutdown_via_agent() {
    let harness = Harness::new("pwr-ok");
    let name = harness.name.clone();
    let config = harness.write_config_with(
        "virtio-devices",
        1,
        "cmdline = \"ternvale.agent=info\"\n\n[vsock]\ncid = 3\nserial = false\n\n[power]\ngraceful_timeout_secs = 15\n",
    );
    let mut vm = harness.spawn_run(&config);
    vm.wait_serial("the agent connecting", BOOT, |serial| {
        serial.contains("agent connected to host")
    });
    wait_state(&harness, "running", Duration::from_secs(20));

    let stop = harness.tv(&["stop", &name, "--graceful", "--no-wait"]);
    assert_eq!(stop.code, Some(0), "{stop:?}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if vm.try_wait().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            tracing::warn!(
                target: T,
                "ternvale run still up after guest SystemOff (host teardown); Drop will kill it"
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        vm.serial()
            .contains("host requested shutdown; syncing and powering off"),
        "guest serial was not a clean agent power-off:\n{}",
        vm.serial()
    );
    let timeout = host_lines(&harness, "graceful-stop timed out; force-stop");
    assert!(
        timeout.is_empty(),
        "force-stop fallback ran on a cooperating guest: {timeout:?}"
    );
    let accepted = host_lines(&harness, "guest accepted graceful-stop");
    assert!(!accepted.is_empty(), "missing guest-accepted log");
    let shutdown = host_lines(&harness, "sent agent shutdown");
    assert!(!shutdown.is_empty(), "missing agent shutdown log");
    tracing::info!(target: T, "Linux graceful stop: clean agent power-off, not force-kill");
}

#[test]
#[ignore = "needs-hv"]
fn linux_graceful_stop_force_stops_when_guest_ignores_power_button() {
    let harness = Harness::new("pwr-ign");
    let name = harness.name.clone();
    let config = harness.write_config_with("", 1, "[power]\ngraceful_timeout_secs = 2\n");
    let mut vm = harness.spawn_run(&config);
    vm.wait_serial("guest init", BOOT, |serial| {
        serial.contains("Run /init as init process")
    });
    wait_state(&harness, "running", Duration::from_secs(20));

    let started = Instant::now();
    let stop = harness.tv(&["stop", &name, "--graceful"]);
    assert_eq!(stop.code, Some(0), "{stop:?}");
    let _ = vm.wait_exit(Duration::from_secs(20));
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "force-stop ran before the timeout: {:?}",
        started.elapsed()
    );
    let lines = host_lines(&harness, "graceful-stop timed out; force-stop");
    assert!(!lines.is_empty(), "force-stop fallback was not logged");
    tracing::info!(target: T, waited_ms = started.elapsed().as_millis() as u64, "busybox ignored KEY_POWER; force-stop ran");
}

#[test]
#[ignore = "needs-hv"]
fn windows_graceful_stop_skipped_without_installed_disk() {
    match std::env::var("TERNVALE_WINDOWS_DISK") {
        Ok(path) if std::path::Path::new(&path).is_file() => {
            tracing::warn!(
                target: T,
                disk = %path,
                "TERNVALE_WINDOWS_DISK is set; Windows graceful-stop still skipped here (no automated in-guest shutdown log parser for a full Windows boot in this test binary)"
            );
        }
        _ => tracing::info!(
            target: T,
            "SKIP Windows stop --graceful: TERNVALE_WINDOWS_DISK not set"
        ),
    }
}
