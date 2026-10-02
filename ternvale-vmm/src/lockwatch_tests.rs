use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use super::{LockStats, LockWatch};

fn spawn_named<F>(name: &str, body: F) -> std::thread::JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(body)
        .expect("spawn")
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn an_uncontended_lock_is_not_registered() {
    let watch = LockWatch::manual(Duration::from_millis(50));
    let mutex = Mutex::new(7);
    *watch.lock(&mutex, "plain") += 1;
    assert_eq!(*watch.lock(&mutex, "plain"), 8);
    assert_eq!(watch.stats(), LockStats::default());
    assert!(watch.scan().is_empty());
}

#[test]
fn a_short_contended_wait_is_counted_but_not_reported() {
    let watch = LockWatch::manual(Duration::from_secs(5));
    let mutex = Arc::new(Mutex::new(0));
    let held = mutex.lock().expect("hold");
    let (waiter_watch, waiter_mutex) = (watch.clone(), Arc::clone(&mutex));
    let waiter = spawn_named("short-waiter", move || {
        *waiter_watch.lock(&waiter_mutex, "short") += 1;
    });
    wait_until("the waiter to register", || !watch.state().waits.is_empty());
    drop(held);
    waiter.join().expect("join");
    let stats = watch.stats();
    assert_eq!((stats.contended, stats.long_waits), (1, 0));
    assert!(stats.max_wait_us > 0);
    assert!(watch.scan().is_empty());
    assert_eq!(*mutex.lock().expect("value"), 1);
}

#[test]
fn a_long_wait_is_reported_while_the_thread_is_still_blocked() {
    let watch = LockWatch::manual(Duration::from_millis(40));
    let mutex = Arc::new(Mutex::new(()));
    let held = mutex.lock().expect("hold");
    let (waiter_watch, waiter_mutex) = (watch.clone(), Arc::clone(&mutex));
    let waiter = spawn_named("stuck-waiter", move || {
        drop(waiter_watch.lock(&waiter_mutex, "bus-slot"));
    });
    std::thread::sleep(Duration::from_millis(100));
    let reports = watch.scan();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0].lock, "bus-slot");
    assert_eq!(reports[0].thread, "stuck-waiter");
    assert!(reports[0].waited >= Duration::from_millis(40));
    assert!(
        watch.scan().is_empty(),
        "the next report waits for the doubling"
    );
    drop(held);
    waiter.join().expect("join");
    let stats = watch.stats();
    assert_eq!((stats.contended, stats.long_waits), (1, 1));
}

/// Two threads take two locks in opposite order and deadlock for good. The
/// threads and mutexes are leaked; the test process exits around them.
#[test]
fn a_lock_order_deadlock_reports_both_waiters() {
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-lockwatch", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let guard =
        ternvale_log::init(ternvale_log::LogConfig::new("lockwatch", dir.clone())).expect("log");
    let watch = LockWatch::manual(Duration::from_millis(40));
    let a: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
    let b: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
    let both_hold_one = Arc::new(Barrier::new(2));
    for (name, first, second, second_name) in [
        ("deadlock-ab", a, b, "lock-b"),
        ("deadlock-ba", b, a, "lock-a"),
    ] {
        let (watch, barrier) = (watch.clone(), Arc::clone(&both_hold_one));
        spawn_named(name, move || {
            let _first = watch.lock(first, "first");
            barrier.wait();
            let _second = watch.lock(second, second_name);
        });
    }
    std::thread::sleep(Duration::from_millis(100));
    let mut reports: Vec<(String, String)> = watch
        .scan()
        .into_iter()
        .map(|report| (report.thread, report.lock))
        .collect();
    reports.sort();
    assert_eq!(
        reports,
        vec![
            ("deadlock-ab".to_string(), "lock-b".to_string()),
            ("deadlock-ba".to_string(), "lock-a".to_string()),
        ]
    );
    assert_eq!(watch.stats().long_waits, 2);

    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    assert!(
        text.contains("lock wait exceeds threshold; possible deadlock"),
        "{text}"
    );
    assert!(text.contains("deadlock-ab on lock-b"), "{text}");
    assert!(text.contains("deadlock-ba on lock-a"), "{text}");
    let sample = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/ternvale-log-samples/lockwatch-deadlock.log");
    if let Some(parent) = sample.parent() {
        std::fs::create_dir_all(parent).expect("sample dir");
    }
    std::fs::write(&sample, &text).expect("sample");
    std::fs::remove_dir_all(&dir).expect("remove log dir");
}

#[test]
fn the_monitor_reports_by_itself_and_exits_when_idle() {
    let watch = LockWatch::new(Duration::from_millis(30));
    let mutex = Arc::new(Mutex::new(()));
    let held = mutex.lock().expect("hold");
    let (waiter_watch, waiter_mutex) = (watch.clone(), Arc::clone(&mutex));
    let waiter = spawn_named("monitored-waiter", move || {
        drop(waiter_watch.lock(&waiter_mutex, "watched"));
    });
    wait_until("the monitor to report", || watch.stats().long_waits == 1);
    assert!(watch.monitor_running());
    drop(held);
    waiter.join().expect("join");
    wait_until("the monitor to exit", || !watch.monitor_running());
    assert_eq!(
        watch.stats().long_waits,
        1,
        "reported once, not again at release"
    );
}

#[test]
fn a_poisoned_lock_is_recovered() {
    let watch = LockWatch::manual(Duration::from_millis(50));
    let mutex = Arc::new(Mutex::new(5));
    let poisoner = Arc::clone(&mutex);
    let result = std::thread::spawn(move || {
        let _guard = poisoner.lock().expect("lock");
        panic!("poison the test mutex");
    })
    .join();
    assert!(result.is_err());
    assert!(mutex.is_poisoned());
    assert_eq!(*watch.lock(&mutex, "poisoned"), 5);
}
