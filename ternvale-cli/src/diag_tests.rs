use std::path::{Path, PathBuf};
use std::sync::Arc;

use ternvale_vmm::VmControl;

use super::{
    read_manifest, render_mmio, sidecar, write_manifest, DiagSink, RunManifest, SharedDevices,
    DTB_SUFFIX, MMIO_SUFFIX, SUMMARY_SUFFIX,
};
use crate::protocol::Response;
use crate::server::{handle_line, handle_line_with};

fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ternvale-cli-diag-{}-{test}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear leftovers");
    }
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

fn sink(dir: &Path, control: &Arc<VmControl>) -> (PathBuf, DiagSink) {
    let log = dir.join("ternvale-demo-20261002-120000.log");
    let sink = DiagSink::new(&log, Arc::clone(control), None, SharedDevices::default());
    (log, sink)
}

#[test]
fn sidecars_share_the_log_stem() {
    let log = Path::new("/l/ternvale-demo-20261002-120000.log");
    assert_eq!(
        sidecar(log, ".run.json"),
        Path::new("/l/ternvale-demo-20261002-120000.run.json")
    );
    assert_eq!(
        sidecar(log, DTB_SUFFIX),
        Path::new("/l/ternvale-demo-20261002-120000.dtb")
    );
}

#[test]
fn manifest_round_trips() {
    let dir = scratch("manifest");
    let manifest = RunManifest {
        name: "demo".into(),
        pid: 42,
        version: "0.1.0".into(),
        config: dir.join("vm.toml"),
        serial_log: dir.join("serial.log"),
        host_log: dir.join("ternvale-demo-20261002-120000.log"),
        started: "2026-10-02T12:00:00+05:30".into(),
    };
    let path = write_manifest(&manifest).expect("write");
    assert_eq!(path, dir.join("ternvale-demo-20261002-120000.run.json"));
    assert_eq!(read_manifest(&path).expect("read"), manifest);
    std::fs::write(&path, "{").expect("corrupt");
    let error = read_manifest(&path).expect_err("corrupt manifest");
    assert!(
        format!("{error:#}").contains("parse run manifest"),
        "{error:#}"
    );
}

#[test]
fn dump_writes_dtb_mmio_and_summary() {
    let dir = scratch("dump");
    let control = Arc::new(VmControl::new("demo", 1));
    let (log, sink) = sink(&dir, &control);
    let files = sink.dump(None).expect("dump without dtb");
    assert_eq!(
        files,
        [sidecar(&log, MMIO_SUFFIX), sidecar(&log, SUMMARY_SUFFIX)]
    );
    control.diagnostics().set_dtb(&[0xd0, 0x0d, 0xfe, 0xed]);
    let files = sink
        .dump(Some(Ok(ternvale_vmm::ExitReason::SystemOff)))
        .expect("dump");
    assert_eq!(files.len(), 3);
    assert_eq!(
        std::fs::read(sidecar(&log, DTB_SUFFIX)).expect("dtb"),
        [0xd0, 0x0d, 0xfe, 0xed]
    );
    let summary = std::fs::read_to_string(sidecar(&log, SUMMARY_SUFFIX)).expect("summary");
    assert!(
        summary.starts_with("vm demo stopped: guest powered off (PSCI SYSTEM_OFF)\n"),
        "{summary}"
    );
    let mmio = std::fs::read_to_string(sidecar(&log, MMIO_SUFFIX)).expect("mmio");
    assert!(
        mmio.starts_with("# vm demo: 0 mmio accesses, 0 unmapped"),
        "{mmio}"
    );
}

#[test]
fn mmio_text_lists_windows_and_events() {
    let control = VmControl::new("demo", 1);
    let text = render_mmio(&control);
    assert_eq!(text.lines().count(), 3, "{text}");
    assert!(text.contains("# seq time_us cpu dir gpa size value device+offset"));
}

#[test]
fn control_socket_dump_returns_the_files() {
    let dir = scratch("socket");
    let control = Arc::new(VmControl::new("demo", 1));
    let (log, sink) = sink(&dir, &control);
    let response: Response =
        handle_line_with(r#"{"cmd":"dump-diagnostics"}"#, &control, None, Some(&sink));
    assert!(response.ok, "{response:?}");
    let files = response.files.expect("files");
    assert_eq!(files.len(), 2);
    assert!(files[0].ends_with(".mmio.txt"), "{files:?}");
    assert!(sidecar(&log, SUMMARY_SUFFIX).exists());
    assert_eq!(response.status.map(|s| s.name), Some("demo".to_string()));

    let without = handle_line(r#"{"cmd":"dump-diagnostics"}"#, &control, None);
    assert!(!without.ok);
    assert_eq!(
        without.error.as_deref(),
        Some("this vm does not record diagnostics")
    );
}

#[test]
fn dump_failure_is_a_response_error() {
    let dir = scratch("unwritable");
    let control = Arc::new(VmControl::new("demo", 1));
    let log = dir
        .join("missing-subdir")
        .join("ternvale-demo-20261002-120000.log");
    let sink = DiagSink::new(&log, Arc::clone(&control), None, SharedDevices::default());
    let response = handle_line_with(r#"{"cmd":"dump-diagnostics"}"#, &control, None, Some(&sink));
    assert!(!response.ok);
    let error = response.error.expect("error");
    assert!(
        error.starts_with("dump diagnostics: write mmio events"),
        "{error}"
    );
}
