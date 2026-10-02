//! End-to-end control of a real VM through the `ternvale` binary.
//!
//! `ternvale run` boots `test-assets/{Image,initramfs.cpio}` in the
//! background; the guest runs `while true; do date; sleep 1; done`. Then
//! `status`, `pause` (the date lines stop and guest time does not move),
//! `resume`, `stop`, and invalid requests on a stopping/stopped VM.
//! Every state transition from the host log is logged and checked.
//!
//! ```sh
//! cargo test -p ternvale-cli --test control -- --ignored --nocapture --test-threads=1
//! ```
//! Artifacts: `target/control-logs/<secs>-<test>/`.

mod support;

use std::time::{Duration, Instant};

use support::{transitions, Harness, Held};

const T: &str = "ternvale::cli";
const BOOT: Duration = Duration::from_secs(60);
const PAUSED_FOR: Duration = Duration::from_secs(4);

/// `Fri Oct  2 15:20:01 UTC 2026` lines printed by busybox `date`.
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

/// Seconds since midnight of a `date` line's `HH:MM:SS`.
fn clock_seconds(line: &str) -> i64 {
    let words: Vec<&str> = line.split_whitespace().collect();
    let hms: Vec<i64> = words[words.len() - 3]
        .split(':')
        .map(|part| part.parse().expect("clock field"))
        .collect();
    hms[0] * 3600 + hms[1] * 60 + hms[2]
}

/// Per-vCPU `runs` from `status --stats --json`.
fn runs(harness: &Harness) -> Vec<u64> {
    let out = harness.tv(&["status", &harness.name, "--stats", "--json"]);
    assert_eq!(out.code, Some(0), "{out:?}");
    let response: ternvale_cli::protocol::Response =
        serde_json::from_str(out.stdout.trim()).expect("stats json");
    response
        .stats
        .expect("stats")
        .cpus
        .iter()
        .map(|cpu| cpu.runs.unwrap_or(0))
        .collect()
}

fn boot(harness: &Harness, cpus: u32) -> support::Vm {
    let config = harness.write_config(cpus);
    let validate = harness.tv(&["validate", &config.display().to_string()]);
    assert_eq!(validate.code, Some(0), "{validate:?}");
    let mut vm = harness.spawn_run(&config);
    vm.wait_serial("shell prompt", BOOT, |serial| serial.contains("# "));
    assert!(
        harness.socket().exists(),
        "no control socket at {}",
        harness.socket().display()
    );
    vm
}

fn expect_transitions(harness: &Harness, want: &[(&str, &str)]) -> Vec<String> {
    let host_log = harness.host_log();
    let found = transitions(&host_log);
    let mut shown = Vec::new();
    for (_, _, line) in &found {
        tracing::info!(target: T, "transition: {line}");
        shown.push(line.clone());
    }
    std::fs::write(harness.out.join("transitions.txt"), shown.join("\n") + "\n")
        .expect("write transitions.txt");
    let pairs: Vec<(&str, &str)> = found
        .iter()
        .map(|(f, t, _)| (f.as_str(), t.as_str()))
        .collect();
    assert_eq!(pairs, want, "host log {}", host_log.display());
    shown
}

#[test]
#[ignore = "needs-hv"]
fn status_pause_resume_stop_and_invalid_transitions() {
    let harness = Harness::new("pause");
    let name = harness.name.clone();
    let mut vm = boot(&harness, 2);

    let status = harness.tv(&["status", &name]);
    assert_eq!(status.code, Some(0), "{status:?}");
    assert!(
        status.stdout.starts_with(&format!("vm {name}: running")),
        "{status:?}"
    );

    // `/bin/busybox <applet>`: this initramfs has no applet links.
    vm.type_chunks(&[
        "while true; do ",
        "/bin/busybox ",
        "date; ",
        "/bin/busybox ",
        "sleep 1; done\n",
    ]);
    vm.wait_serial("three date lines", Duration::from_secs(15), |s| {
        date_lines(s).len() >= 3
    });

    let paused = harness.tv(&["pause", &name]);
    assert_eq!(paused.code, Some(0), "{paused:?}");
    assert!(
        paused.stdout.starts_with(&format!("vm {name}: paused")),
        "{paused:?}"
    );
    let paused_at = Instant::now();
    std::thread::sleep(Duration::from_millis(300));
    let before = date_lines(&vm.serial());
    let runs_before = runs(&harness);
    std::thread::sleep(PAUSED_FOR);
    let during = date_lines(&vm.serial());
    let runs_during = runs(&harness);
    tracing::info!(target: T, lines_at_pause = before.len(), lines_after_wait = during.len(), ?runs_before, ?runs_during, wait_ms = PAUSED_FOR.as_millis() as u64, "guest while paused");
    assert_eq!(before, during, "the guest kept printing while paused");
    assert_eq!(
        runs_before, runs_during,
        "vCPUs entered the guest while paused"
    );

    let again = harness.tv(&["pause", &name]);
    assert_eq!(
        again.code,
        Some(0),
        "pausing a paused vm is a no-op: {again:?}"
    );
    assert!(again.stdout.contains("over 1 pause(s)"), "{again:?}");

    let resumed = harness.tv(&["resume", &name]);
    assert_eq!(resumed.code, Some(0), "{resumed:?}");
    assert!(
        resumed.stdout.starts_with(&format!("vm {name}: running")),
        "{resumed:?}"
    );
    let count = during.len();
    vm.wait_serial("a date line after resume", Duration::from_secs(5), |s| {
        date_lines(s).len() > count
    });
    let after = date_lines(&vm.serial());
    let (last_before, first_after) = (&during[count - 1], &after[count]);
    let guest_gap = (clock_seconds(first_after) - clock_seconds(last_before)).rem_euclid(86_400);
    let wall_gap = paused_at.elapsed().as_secs();
    tracing::info!(target: T, last_before = %last_before, first_after = %first_after, guest_gap_s = guest_gap, wall_since_pause_s = wall_gap, "guest clock across the pause");
    assert!(
        guest_gap <= 2,
        "guest time jumped {guest_gap}s across a {wall_gap}s pause"
    );
    assert!(wall_gap >= PAUSED_FOR.as_secs());
    assert!(
        runs(&harness) != runs_during,
        "vCPUs did not run after resume"
    );
    let again = harness.tv(&["resume", &name]);
    assert_eq!(
        again.code,
        Some(0),
        "resuming a running vm is a no-op: {again:?}"
    );

    // Hold a connection across `stop` so requests can reach the VM while it stops.
    let mut held = Held::connect(&harness.socket());
    let stop = harness.tv_spawn(&["stop", &name]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let state = loop {
        let response = held
            .send(r#"{"cmd":"status"}"#)
            .expect("vm closed before stopping");
        let state = response.status.expect("status").state;
        if state != "running" {
            break state;
        }
        assert!(
            Instant::now() < deadline,
            "vm never left running after stop"
        );
    };
    tracing::info!(target: T, state = %state, "vm left running after stop");
    let refused = held
        .send(r#"{"cmd":"pause"}"#)
        .expect("vm closed before the pause was answered");
    assert!(!refused.ok, "{refused:?}");
    let error = refused.error.unwrap_or_default();
    assert!(
        error.starts_with("cannot pause a vm that is stopping;")
            || error.starts_with("cannot pause a vm that is stopped;"),
        "{error}"
    );
    tracing::info!(target: T, error = %error, "pause while stopping refused over the socket");

    let stop = stop.wait_with_output().expect("wait for ternvale stop");
    let stop_stdout = String::from_utf8_lossy(&stop.stdout);
    tracing::info!(target: T, code = ?stop.status.code(), stdout = %stop_stdout.trim_end(), "ternvale stop finished");
    assert_eq!(stop.status.code(), Some(0), "{stop:?}");
    assert!(
        stop_stdout.contains(&format!("vm {name}: exited")),
        "{stop_stdout}"
    );
    let exit = vm.wait_exit(Duration::from_secs(10));
    assert_eq!(exit.code(), Some(0), "ternvale run must exit cleanly");
    assert!(!harness.socket().exists(), "socket left behind");

    let socket = harness.socket().display().to_string();
    for (args, verb) in [
        (["pause", &name], "pause"),
        (["resume", &name], "resume"),
        (["stop", &name], "stop"),
    ] {
        let out = harness.tv(&args);
        assert_eq!(out.code, Some(1), "{out:?}");
        assert_eq!(
            out.error_line(),
            format!(
                "error: cannot {verb} vm {name}: it is not running (no control socket at {socket})"
            )
        );
    }
    let gone = harness.tv(&["status", &name]);
    assert_eq!(
        (gone.code, gone.stdout.trim()),
        (Some(1), format!("vm {name}: not running").as_str())
    );

    expect_transitions(
        &harness,
        &[
            ("created", "running"),
            ("running", "paused"),
            ("paused", "running"),
            ("running", "stopping"),
            ("stopping", "stopped"),
        ],
    );
    let host = std::fs::read_to_string(harness.host_log()).expect("host log");
    let rejected: Vec<String> = host
        .lines()
        .map(support::strip_ansi)
        .filter(|line| line.contains("control request rejected"))
        .collect();
    for line in &rejected {
        tracing::info!(target: T, "rejection: {line}");
    }
    assert!(
        rejected.iter().any(|line| line.contains("op=\"pause\"")),
        "no rejected pause in the host log"
    );
}

#[test]
#[ignore = "needs-hv"]
fn force_stop_exits_cleanly() {
    let harness = Harness::new("force");
    let name = harness.name.clone();
    let mut vm = boot(&harness, 1);
    let stop = harness.tv(&["stop", &name, "--force"]);
    assert_eq!(stop.code, Some(0), "{stop:?}");
    assert!(stop.stdout.contains("stop cause: force-stop"), "{stop:?}");
    assert!(
        stop.stdout.contains(&format!("vm {name}: exited")),
        "{stop:?}"
    );
    assert_eq!(vm.wait_exit(Duration::from_secs(10)).code(), Some(0));
    let lines = expect_transitions(
        &harness,
        &[
            ("created", "running"),
            ("running", "stopping"),
            ("stopping", "stopped"),
        ],
    );
    assert!(
        lines[1].contains("why=\"force-stop requested\""),
        "{}",
        lines[1]
    );
}
