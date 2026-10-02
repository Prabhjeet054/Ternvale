use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use super::tests::{spawn_named, wait_until};
use super::{LockWatch, WaitKind};

struct Flag {
    mutex: Mutex<bool>,
    changed: Condvar,
}

impl Flag {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            mutex: Mutex::new(false),
            changed: Condvar::new(),
        })
    }

    fn set(&self, watch: &LockWatch) {
        *watch.lock(&self.mutex, "flag") = true;
        self.changed.notify_all();
    }
}

#[test]
fn a_condvar_wait_releases_the_hold_and_takes_it_back() {
    let watch = LockWatch::manual(Duration::from_secs(5));
    let flag = Flag::new();
    let (thread_watch, thread_flag) = (watch.clone(), Arc::clone(&flag));
    let waiter = spawn_named("cv-waiter", move || {
        let mut guard = thread_watch.lock(&thread_flag.mutex, "flag");
        while !*guard {
            guard = guard.wait(&thread_flag.changed);
        }
        assert_eq!(thread_watch.held_by("cv-waiter"), vec!["flag"]);
    });
    wait_until("the condvar wait", || {
        watch.waits() == vec![("cv-waiter".into(), "flag".into(), WaitKind::Condvar)]
    });
    assert!(
        watch.held_by("cv-waiter").is_empty(),
        "released while waiting"
    );
    flag.set(&watch);
    waiter.join().expect("join");
    assert!(watch.waits().is_empty());
    assert_eq!(
        watch.stats().contended,
        0,
        "a condvar wait is not lock contention"
    );
}

#[test]
fn a_long_brief_wait_is_reported_and_a_parked_one_is_only_context() {
    let dir = std::env::temp_dir().join(format!("ternvale-log-{}-condvar", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let guard = crate::init(crate::LogConfig::new("condvar", dir.clone())).expect("log");
    let watch = LockWatch::manual(Duration::from_millis(40));
    let flag = Flag::new();
    let mut threads = Vec::new();
    for name in ["cv-brief", "cv-parked"] {
        let (thread_watch, thread_flag) = (watch.clone(), Arc::clone(&flag));
        threads.push(spawn_named(name, move || {
            let mut guard = thread_watch.lock(&thread_flag.mutex, "flag");
            while !*guard {
                guard = if name == "cv-parked" {
                    guard.park(&thread_flag.changed)
                } else {
                    guard.wait(&thread_flag.changed)
                };
            }
        }));
    }
    wait_until("both waits", || watch.waits().len() == 2);
    std::thread::sleep(Duration::from_millis(100));
    let scan = watch.scan();
    assert_eq!(scan.long_waits.len(), 1, "{scan:?}");
    let report = &scan.long_waits[0];
    assert_eq!(
        (report.thread.as_str(), report.kind),
        ("cv-brief", WaitKind::Condvar)
    );
    assert_eq!(report.holder, None);
    assert!(scan.deadlocks.is_empty());
    flag.set(&watch);
    for thread in threads {
        thread.join().expect("join");
    }
    assert_eq!(watch.stats().long_waits, 1);

    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    for needle in [
        "condvar wait exceeds threshold",
        "cv-brief waiting on flag condvar for",
        "cv-parked parked on flag for",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
    }
    std::fs::remove_dir_all(&dir).expect("remove log dir");
}

#[test]
fn wait_timeout_reports_the_timeout_and_leaves_no_wait() {
    let watch = LockWatch::manual(Duration::from_secs(5));
    let flag = Flag::new();
    let guard = watch.lock(&flag.mutex, "flag");
    let (guard, timed_out) = guard.wait_timeout(&flag.changed, Duration::from_millis(5));
    assert!(timed_out);
    assert!(!*guard);
    drop(guard);
    assert!(watch.waits().is_empty());
}

#[test]
fn a_parked_wait_does_not_start_the_monitor() {
    let watch = LockWatch::new(Duration::from_millis(20));
    let flag = Flag::new();
    let (thread_watch, thread_flag) = (watch.clone(), Arc::clone(&flag));
    let parked = spawn_named("cv-idle", move || {
        let mut guard = thread_watch.lock(&thread_flag.mutex, "flag");
        while !*guard {
            guard = guard.park(&thread_flag.changed);
        }
    });
    wait_until("the park", || !watch.waits().is_empty());
    std::thread::sleep(Duration::from_millis(60));
    assert!(!watch.monitor_running());
    flag.set(&watch);
    parked.join().expect("join");
    assert_eq!(watch.stats().long_waits, 0);
}
