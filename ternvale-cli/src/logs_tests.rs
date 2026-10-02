use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{check_targets, find_logs, follow, newest, print_file, stamp_of};
use crate::logfmt::{Filter, Level, LineFilter};

fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ternvale-cli-logs-{}-{test}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear leftovers");
    }
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

const SAMPLE: &str = "\
2026-10-02T16:00:00Z  INFO ThreadId(01) vm{\u{1b}[3mvm\u{1b}[0m=demo}: ternvale::cli: ternvale run
2026-10-02T16:00:01Z DEBUG ThreadId(02) ternvale::virtio::blk: queue notify
2026-10-02T16:00:02Z  WARN ThreadId(02) ternvale::virtio::net: dropped frame
2026-10-02T16:00:03Z ERROR ThreadId(01) ternvale::log: panic at x
   0: backtrace frame
2026-10-02T16:00:04Z  INFO ThreadId(01) ternvale::agent: agent connected
";

fn run_print(dir: &std::path::Path, filter: Filter, color: bool) -> String {
    let path = dir.join("ternvale-demo-20261002-160000.log");
    std::fs::write(&path, SAMPLE).expect("log");
    let mut out = Vec::new();
    print_file(&path, LineFilter::new(filter), &mut out, color).expect("print");
    String::from_utf8(out).expect("utf8")
}

#[test]
fn log_names_match_exactly() {
    assert_eq!(
        stamp_of("ternvale-demo-20261002-160000.log", "demo"),
        Some("20261002-160000")
    );
    assert_eq!(
        stamp_of(
            "ternvale-it-agentfs-3441-20261002-213307.log",
            "it-agentfs-3441"
        ),
        Some("20261002-213307")
    );
    for (name, vm) in [
        ("ternvale-demo-2-20261002-160000.log", "demo"),
        ("ternvale-demo-20261002-160000.run.json", "demo"),
        ("ternvale-demo-20261002-160000.summary.txt", "demo"),
        ("ternvale-demo-2026100-160000.log", "demo"),
        ("ternvale-demo-20261002-16000x.log", "demo"),
        ("ternvale-cli-20261002-160000.log", "demo"),
        ("demo-20261002-160000.log", "demo"),
    ] {
        assert_eq!(stamp_of(name, vm), None, "{name}");
    }
}

#[test]
fn finds_a_vms_logs_oldest_first() {
    let dir = scratch("find");
    for name in [
        "ternvale-demo-20261002-160000.log",
        "ternvale-demo-20251231-235959.log",
        "ternvale-demo-20261002-160000.mmio.txt",
        "ternvale-demo-2-20261003-000000.log",
        "ternvale-demo-20261002-170000.log",
    ] {
        std::fs::write(dir.join(name), b"").expect("file");
    }
    let found = find_logs(&dir, "demo").expect("find");
    let names: Vec<_> = found
        .iter()
        .map(|p| p.file_name().expect("name").to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "ternvale-demo-20251231-235959.log",
            "ternvale-demo-20261002-160000.log",
            "ternvale-demo-20261002-170000.log"
        ]
    );
    assert_eq!(
        newest(&dir, "demo-2").expect("newest"),
        Some(dir.join("ternvale-demo-2-20261003-000000.log"))
    );
    assert_eq!(newest(&dir, "other").expect("none"), None);
    assert!(find_logs(&dir.join("absent"), "demo").is_err());
}

#[test]
fn prints_everything_without_filters_and_strips_color() {
    let dir = scratch("all");
    let text = run_print(&dir, Filter::default(), false);
    assert_eq!(text.lines().count(), 6);
    assert!(
        text.starts_with("2026-10-02T16:00:00Z  INFO ThreadId(01) vm{vm=demo}: ternvale::cli"),
        "{text}"
    );
    let colored = run_print(&dir, Filter::default(), true);
    assert!(colored.contains('\u{1b}'));
}

#[test]
fn filters_by_level_and_target() {
    let dir = scratch("filter");
    let warn = run_print(
        &dir,
        Filter {
            level: Some(Level::Warn),
            targets: Vec::new(),
        },
        false,
    );
    assert_eq!(
        warn,
        "2026-10-02T16:00:02Z  WARN ThreadId(02) ternvale::virtio::net: dropped frame\n\
         2026-10-02T16:00:03Z ERROR ThreadId(01) ternvale::log: panic at x\n   0: backtrace frame\n"
    );
    let virtio = run_print(
        &dir,
        Filter {
            level: None,
            targets: vec!["virtio".into()],
        },
        false,
    );
    assert_eq!(virtio.lines().count(), 2, "{virtio}");
    assert!(virtio.lines().all(|l| l.contains("ternvale::virtio::")));
    let both = run_print(
        &dir,
        Filter {
            level: Some(Level::Info),
            targets: vec!["ternvale::virtio::blk".into(), "agent".into()],
        },
        false,
    );
    assert_eq!(
        both,
        "2026-10-02T16:00:04Z  INFO ThreadId(01) ternvale::agent: agent connected\n"
    );
}

#[test]
fn bad_targets_are_rejected() {
    assert!(check_targets(&["ternvale::virtio".into()]).is_ok());
    assert!(check_targets(&[String::new()]).is_err());
    assert!(check_targets(&["a b".into()]).is_err());
}

#[derive(Clone, Default)]
struct Shared(Arc<std::sync::Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("buf").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("buf").clone()).expect("utf8")
    }

    fn wait_for(&self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.text().contains(needle) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {needle:?}; got {:?}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn follow_streams_appends_truncation_and_a_newer_log() {
    let dir = scratch("follow");
    let first = dir.join("ternvale-demo-20261002-160000.log");
    std::fs::write(&first, "2026-10-02T16:00:00Z  INFO ternvale::cli: one\n").expect("log");
    let out = Shared::default();
    let stop = Arc::new(AtomicBool::new(false));
    let follower = {
        let (dir, first, out, stop) = (dir.clone(), first.clone(), out.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut out = out;
            follow(
                &first,
                Some((dir, "demo".into())),
                LineFilter::new(Filter {
                    level: Some(Level::Info),
                    targets: Vec::new(),
                }),
                &mut out,
                false,
                &|| stop.load(Ordering::SeqCst),
            )
        })
    };
    out.wait_for("one\n");

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&first)
        .expect("append");
    file.write_all(b"2026-10-02T16:00:01Z DEBUG ternvale::cli: hidden\n2026-10-02T16:00:02Z  INFO ternvale::cli: tw")
        .expect("partial");
    file.flush().expect("flush");
    std::thread::sleep(Duration::from_millis(600));
    assert!(
        !out.text().contains("tw"),
        "a partial line waits for its newline"
    );
    file.write_all(b"o\n").expect("rest");
    out.wait_for("two\n");
    assert!(!out.text().contains("hidden"));

    std::fs::write(&first, "2026-10-02T16:00:03Z  INFO ternvale::cli: 3\n").expect("truncate");
    out.wait_for(": 3\n");

    let second = dir.join("ternvale-demo-20261002-170000.log");
    std::fs::write(
        &second,
        "2026-10-02T17:00:00Z  INFO ternvale::cli: restarted\n",
    )
    .expect("new log");
    out.wait_for("restarted\n");

    stop.store(true, Ordering::SeqCst);
    follower.join().expect("join").expect("follow ok");
    let lines: Vec<String> = out
        .text()
        .lines()
        .map(|l| l.rsplit(": ").next().unwrap_or("").to_string())
        .collect();
    assert_eq!(lines, ["one", "two", "3", "restarted"]);
}
