//! End-to-end checks of the debugging tools through the `ternvale` binary.
//!
//! `doctor_passes_signed_and_fails_without_the_entitlement` runs
//! `ternvale doctor` on the signed binary (every check passes) and on a copy
//! ad-hoc signed without entitlements (hypervisor and entitlement fail with
//! the codesign fix; GIC is skipped; exit 1).
//!
//! `summary_report_and_logs_for_a_real_run` boots the `virtio-devices`
//! initramfs with the guest agent and a loopback NIC, bundles a report while
//! it runs (`dump-diagnostics`), stops it, checks the shutdown summary on
//! stderr, bundles a report from the stop-time files, and filters its log.
//!
//! ```sh
//! cargo test -p ternvale-cli --test diagnostics -- --ignored --nocapture
//! ```
//! Artifacts: `target/control-logs/<secs>-diag*/`.

mod support;

use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use support::{strip_ansi, Harness};

const T: &str = "ternvale::cli";
const BOOT: Duration = Duration::from_secs(90);

/// `(entry name without the top folder, contents)` of every file in `zip`.
fn unzip(zip: &Path) -> Vec<(String, String)> {
    let file = std::fs::File::open(zip).expect("open report");
    let mut archive = zip::ZipArchive::new(file).expect("read report");
    (0..archive.len())
        .map(|i| {
            let mut entry = archive.by_index(i).expect("entry");
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).expect("read entry");
            let name = entry
                .name()
                .split_once('/')
                .map_or("", |(_, n)| n)
                .to_string();
            (name, String::from_utf8_lossy(&bytes).into_owned())
        })
        .collect()
}

fn entry<'a>(files: &'a [(String, String)], name: &str) -> &'a str {
    files
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, text)| text.as_str())
        .unwrap_or_else(|| {
            panic!(
                "{name} not in report: {:?}",
                files.iter().map(|f| &f.0).collect::<Vec<_>>()
            )
        })
}

#[test]
#[ignore = "needs-hv"]
fn doctor_passes_signed_and_fails_without_the_entitlement() {
    let harness = Harness::new("diagdoc");
    let signed = harness.tv(&["doctor"]);
    assert_eq!(signed.code, Some(0), "{signed:?}");
    for name in [
        "macOS version",
        "hypervisor",
        "entitlement",
        "GICv3",
        "disk space",
        "log directory",
    ] {
        assert!(
            signed
                .stdout
                .lines()
                .any(|l| l.starts_with("PASS  ") && l.contains(name)),
            "{name}: {}",
            signed.stdout
        );
    }
    assert!(signed
        .stdout
        .ends_with("6 passed, 0 warnings, 0 failed, 0 skipped\n"));

    let bare = harness.out.join("ternvale-no-entitlement");
    std::fs::copy(&harness.bin, &bare).expect("copy binary");
    let sign = Command::new("codesign")
        .args(["--sign", "-", "--force"])
        .arg(&bare)
        .output()
        .expect("codesign");
    assert!(
        sign.status.success(),
        "{}",
        String::from_utf8_lossy(&sign.stderr)
    );
    let out = Command::new(&bare)
        .arg("doctor")
        .output()
        .expect("run doctor");
    let stdout = String::from_utf8_lossy(&out.stdout);
    tracing::info!(target: T, code = ?out.status.code(), stdout = %stdout, "doctor without entitlement");
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("FAIL  hypervisor     hv_vm_create returned HV_DENIED"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "FAIL  entitlement    {} lacks com.apple.security.hypervisor",
            bare.display()
        )),
        "{stdout}"
    );
    assert!(stdout.contains(&format!("fix: Sign it: `codesign --sign - --force --entitlements entitlements/ternvale.entitlements {}`", bare.display())), "{stdout}");
    assert!(stdout.contains("SKIP  GICv3"), "{stdout}");
    assert!(
        stdout.ends_with("3 passed, 0 warnings, 2 failed, 1 skipped\n"),
        "{stdout}"
    );
}

#[test]
#[ignore = "needs-hv"]
fn summary_report_and_logs_for_a_real_run() {
    let harness = Harness::new("diag");
    let name = harness.name.clone();
    let config = harness.write_config_with(
        "virtio-devices",
        2,
        "cmdline = \"ternvale.agent=info\"\n\n[[nics]]\nbackend = \"loopback\"\n\n[vsock]\ncid = 3\n",
    );
    let mut vm = harness.spawn_run(&config);
    vm.wait_serial("the agent connecting", BOOT, |serial| {
        serial.contains("agent connected to host")
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while !harness
        .tv(&["status", &name])
        .stdout
        .contains("agent: connected")
    {
        assert!(Instant::now() < deadline, "agent never connected");
        std::thread::sleep(Duration::from_millis(250));
    }

    let live_zip = harness.out.join("live-report.zip");
    let live = harness.tv(&["report", &name, "--out", &live_zip.display().to_string()]);
    assert_eq!(live.code, Some(0), "{live:?}");
    assert!(
        live.stdout
            .contains("vm was running: dump-diagnostics wrote 3 files"),
        "{}",
        live.stdout
    );
    assert!(!live.stdout.contains("Missing:"), "{}", live.stdout);
    let files = unzip(&live_zip);
    assert!(entry(&files, "mmio-events.txt").contains("\nwindow pl011 0x9000000 0x1000 "));
    assert!(entry(&files, "summary.txt")
        .starts_with(&format!("vm {name} is running (snapshot, not stopped)")));
    assert!(entry(&files, "config.toml").contains(&format!("name = \"{name}\"")));
    assert!(entry(&files, "guest-serial.log").contains("Linux version"));
    assert!(entry(&files, "doctor.txt").contains("PASS  hypervisor"));
    assert!(!entry(&files, "guest.dtb").is_empty());

    let stop = harness.tv(&["stop", &name]);
    assert_eq!(stop.code, Some(0), "{stop:?}");
    assert!(vm.wait_exit(Duration::from_secs(30)).success());
    let stderr =
        strip_ansi(&std::fs::read_to_string(harness.out.join("run.stderr")).expect("run.stderr"));
    let summary: Vec<&str> = stderr
        .lines()
        .skip_while(|l| !l.starts_with("ternvale: vm "))
        .take_while(|l| l.starts_with("ternvale: vm ") || l.starts_with("  "))
        .collect();
    tracing::info!(target: T, summary = %summary.join("\n"), "shutdown summary");
    assert_eq!(
        summary.first().copied(),
        Some(
            format!("ternvale: vm {name} stopped: host shutdown request (ternvale stop)").as_str()
        )
    );
    for want in [
        "  uptime ",
        "  vcpu 0: ",
        "  vcpu 1: ",
        "  mmio: ",
        "  nic 0 (loopback): tx ",
        "  vsock: tx ",
        "  agent: ",
    ] {
        assert!(
            summary.iter().any(|l| l.starts_with(want)),
            "{want:?} in {summary:#?}"
        );
    }
    assert!(
        stderr.contains("ternvale::cli: vm summary"),
        "structured summary log"
    );

    let exited_zip = harness.out.join("exited-report.zip");
    let exited = harness.tv(&["report", &name, "--out", &exited_zip.display().to_string()]);
    assert_eq!(exited.code, Some(0), "{exited:?}");
    assert!(exited
        .stdout
        .contains("vm was not running: diagnostics are the ones written when it stopped"));
    let files = unzip(&exited_zip);
    assert!(entry(&files, "summary.txt")
        .starts_with(&format!("vm {name} stopped: host shutdown request")));
    assert!(entry(&files, "README.txt").contains("  guest.dtb: "));

    let warn = harness.tv(&["logs", &name, "--level", "warn"]);
    assert_eq!(warn.code, Some(0), "{warn:?}");
    let agent = harness.tv(&["logs", &name, "--target", "agent"]);
    assert!(
        agent
            .stdout
            .contains("ternvale::agent: agent connected version=1"),
        "{}",
        agent.stdout
    );
    let headers = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|l| l.starts_with("20"))
            .map(str::to_string)
            .collect()
    };
    assert!(
        headers(&agent.stdout)
            .iter()
            .all(|l| l.contains(" ternvale::agent: ")),
        "{}",
        agent.stdout
    );
    assert!(
        headers(&warn.stdout)
            .iter()
            .all(|l| l.contains(" WARN ") || l.contains(" ERROR ")),
        "{}",
        warn.stdout
    );
    let list = harness.tv(&["logs", &name, "--list"]);
    assert_eq!(list.stdout.lines().count(), 1, "{}", list.stdout);
}
