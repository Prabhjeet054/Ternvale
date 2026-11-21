//! Step 117 host analogs: SIGKILL of a VMM child, 3-in-5-minute stop, dump in the report.

use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ternvale_vmm::crash::{
    mark_unclean_shutdown, parse_unclean_shutdown, RAM_DUMP_FILE, RAM_DUMP_PRIVACY_WARNING,
    RAM_DUMP_WARNING_FILE, UNCLEAN_FILE,
};
use ternvale_vmm::{SuperviseDecision, VmmSupervisor};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

use crate::report::{plan, PlanInputs};

struct LogBuf(Arc<Mutex<String>>);

struct FieldSink(String);

impl Visit for FieldSink {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!(" {}={value:?}", field.name()));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push_str(&format!(" {}={value}", field.name()));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.push_str(&format!(" {}={value}", field.name()));
    }
}

impl Subscriber for LogBuf {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == "ternvale::watchdog" || metadata.target() == "ternvale::cli"
    }
    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _span: &Id, _values: &Record<'_>) {}
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut sink = FieldSink(String::new());
        event.record(&mut sink);
        let mut g = self.0.lock().expect("supervisor log");
        g.push_str(event.metadata().level().as_str());
        g.push(' ');
        g.push_str(event.metadata().target());
        g.push_str(&sink.0);
        g.push('\n');
    }
    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}

fn capture(f: impl FnOnce()) -> String {
    let buf = Arc::new(Mutex::new(String::new()));
    tracing::subscriber::with_default(LogBuf(Arc::clone(&buf)), f);
    let logs = buf.lock().expect("supervisor log").clone();
    logs
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tv-cli-rel-{}-{tag}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

/// `kill -9` of a stand-in VMM process: SIGKILL, unclean marker, first restart.
#[test]
fn kill_dash_nine_of_vmm_child_restarts_with_unclean_shutdown_marker() {
    let dir = scratch("kill9");
    let logs = capture(|| {
        let mut child = Command::new("/bin/sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep");
        let pid = child.id();
        // SIGKILL is the same signal as `kill -9`.
        child.kill().expect("SIGKILL");
        let status = child.wait().expect("wait");
        assert!(!status.success(), "{status:?} pid={pid}");
        let path = mark_unclean_shutdown(&dir, "VMM process died (killed: SIGKILL)").expect("mark");
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(UNCLEAN_FILE)
        );
        let bytes = std::fs::read(&path).expect("read marker");
        let (_, reason) = parse_unclean_shutdown(&bytes).expect("parse");
        assert!(reason.contains("SIGKILL"), "{reason}");
        let mut supervisor = VmmSupervisor::new();
        match supervisor.note_crash(Duration::from_secs(0)) {
            SuperviseDecision::Restart { backoff, crash_n } => {
                assert_eq!(crash_n, 1);
                assert_eq!(backoff, Duration::from_secs(1));
            }
            other => panic!("{other:?}"),
        }
    });
    assert!(logs.contains("disk marked unclean-shutdown"), "{logs}");
    assert!(
        logs.contains("vmm process died unexpectedly") || logs.contains("supervisor will restart"),
        "{logs}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Three unexpected exits in the window stop restarts and log a notification.
#[test]
fn three_quick_vmm_crashes_stop_restarts_with_a_notification() {
    let logs = capture(|| {
        let mut supervisor = VmmSupervisor::new();
        assert!(matches!(
            supervisor.note_crash(Duration::from_secs(0)),
            SuperviseDecision::Restart { .. }
        ));
        assert!(matches!(
            supervisor.note_crash(Duration::from_secs(1)),
            SuperviseDecision::Restart { .. }
        ));
        match supervisor.note_crash(Duration::from_secs(2)) {
            SuperviseDecision::Stop { crashes, reason } => {
                assert_eq!(crashes, 3);
                assert!(reason.contains("3 times in 5 minutes"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    });
    assert!(
        logs.contains("supervisor stopping restarts") && logs.contains("3 times in 5 minutes"),
        "{logs}"
    );
}

/// Crash report plan includes the RAM dump, cap, and the UI privacy warning.
#[test]
fn crash_report_includes_capped_dump_and_privacy_warning() {
    let dir = scratch("dump-ui");
    std::fs::write(dir.join("ternvale-demo-20261121-120000.log"), "host\n").expect("log");
    let config = dir.join("config.toml");
    std::fs::write(&config, "name = \"demo\"\n").expect("config");
    let dump = dir.join(RAM_DUMP_FILE);
    let mut dump_bytes = Vec::from(*b"TVRD");
    dump_bytes.extend_from_slice(&1u32.to_le_bytes());
    dump_bytes.extend_from_slice(&0u64.to_le_bytes());
    dump_bytes.extend_from_slice(&(2u64 << 20).to_le_bytes());
    dump_bytes.extend_from_slice(&(64u64 * 1024).to_le_bytes());
    dump_bytes.extend_from_slice(&(1024u64 * 1024).to_le_bytes());
    dump_bytes.extend_from_slice(&[0xABu8; 64 * 1024]);
    std::fs::write(&dump, &dump_bytes).expect("dump");
    std::fs::write(dir.join(RAM_DUMP_WARNING_FILE), RAM_DUMP_PRIVACY_WARNING).expect("warn");
    let planned = plan(
        &dir,
        &PlanInputs {
            name: "demo",
            config: Some(&config),
            live: crate::report::Live::NotRunning,
            doctor: Some("ok".into()),
        },
        "20261121-130000",
    )
    .expect("plan");
    assert!(
        planned
            .header
            .iter()
            .any(|line| line.contains("PRIVACY") && line.contains("secrets")),
        "{:?}",
        planned.header
    );
    let names: Vec<&str> = planned.items.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"memory.dump"), "{names:?}");
    assert!(names.contains(&"memory.dump.WARNING.txt"), "{names:?}");
    let warning = planned
        .items
        .iter()
        .find(|i| i.name == "memory.dump.WARNING.txt")
        .expect("warn item");
    match &warning.source {
        crate::report::bundle::Source::Bytes(bytes) => {
            let text = String::from_utf8_lossy(bytes);
            assert!(
                text.contains("passwords") && text.contains("keys"),
                "{text}"
            );
        }
        other => panic!("{other:?}"),
    }
    let dump_item = planned
        .items
        .iter()
        .find(|i| i.name == "memory.dump")
        .expect("dump item");
    match &dump_item.source {
        crate::report::bundle::Source::File { cap, .. } => {
            assert!(
                *cap <= (ternvale_config::DEFAULT_DUMP_RAM_MAX_MIB << 20),
                "cap {cap}"
            );
        }
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
