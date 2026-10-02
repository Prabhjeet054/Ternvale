use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use super::{Deadlock, LockStats, LockWatch, WaitKind};

pub(super) fn spawn_named<F>(name: &str, body: F) -> std::thread::JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(body)
        .expect("spawn")
}

pub(super) fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn current_name() -> String {
    std::thread::current()
        .name()
        .expect("named test thread")
        .to_string()
}

#[test]
fn an_uncontended_lock_records_its_holder_until_dropped() {
    let watch = LockWatch::manual(Duration::from_millis(50));
    let mutex = Mutex::new(7);
    let me = current_name();
    {
        let mut guard = watch.lock(&mutex, "plain");
        *guard += 1;
        assert_eq!(watch.held_by(&me), vec!["plain".to_string()]);
    }
    assert!(watch.held_by(&me).is_empty());
    assert_eq!(*watch.lock(&mutex, "plain"), 8);
    assert_eq!(watch.stats(), LockStats::default());
    assert_eq!(watch.scan(), Default::default());
}

#[test]
fn nested_holds_are_listed_in_order_and_released() {
    let watch = LockWatch::manual(Duration::from_millis(50));
    let (outer, inner) = (Mutex::new(()), Mutex::new(()));
    let me = current_name();
    let first = watch.lock(&outer, "bus-slot");
    let second = watch.lock(&inner, "serial");
    assert_eq!(watch.held_by(&me), vec!["bus-slot", "serial"]);
    drop(first);
    assert_eq!(watch.held_by(&me), vec!["serial"]);
    drop(second);
    assert!(watch.held_by(&me).is_empty());
}

#[test]
fn a_short_contended_wait_is_counted_but_not_reported() {
    let watch = LockWatch::manual(Duration::from_secs(5));
    let mutex = Arc::new(Mutex::new(0));
    let held = watch.lock(&mutex, "short");
    let (waiter_watch, waiter_mutex) = (watch.clone(), Arc::clone(&mutex));
    let waiter = spawn_named("short-waiter", move || {
        *waiter_watch.lock(&waiter_mutex, "short") += 1;
    });
    wait_until("the waiter to register", || !watch.waits().is_empty());
    drop(held);
    waiter.join().expect("join");
    let stats = watch.stats();
    assert_eq!((stats.contended, stats.long_waits), (1, 0));
    assert!(stats.max_wait_us > 0);
    assert_eq!(watch.scan(), Default::default());
    assert_eq!(*mutex.lock().expect("value"), 1);
}

#[test]
fn a_long_wait_names_the_holder_while_the_waiter_is_blocked() {
    let watch = LockWatch::manual(Duration::from_millis(40));
    let mutex = Arc::new(Mutex::new(()));
    let (holder_watch, holder_mutex) = (watch.clone(), Arc::clone(&mutex));
    let holding = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (held_signal, release_signal) = (Arc::clone(&holding), Arc::clone(&release));
    let holder = spawn_named("slow-holder", move || {
        let _guard = holder_watch.lock(&holder_mutex, "bus-slot");
        held_signal.wait();
        release_signal.wait();
    });
    holding.wait();
    let (waiter_watch, waiter_mutex) = (watch.clone(), Arc::clone(&mutex));
    let waiter = spawn_named("stuck-waiter", move || {
        drop(waiter_watch.lock(&waiter_mutex, "bus-slot"));
    });
    std::thread::sleep(Duration::from_millis(100));
    let scan = watch.scan();
    assert_eq!(scan.long_waits.len(), 1, "{scan:?}");
    let report = &scan.long_waits[0];
    assert_eq!(
        (report.lock.as_str(), report.thread.as_str()),
        ("bus-slot", "stuck-waiter")
    );
    assert_eq!(report.kind, WaitKind::Lock);
    assert!(report.waited >= Duration::from_millis(40));
    let owner = report.holder.as_ref().expect("holder known");
    assert_eq!(owner.thread, "slow-holder");
    assert_eq!(owner.holds, vec!["bus-slot"]);
    assert!(owner.held >= report.waited);
    assert!(scan.deadlocks.is_empty());
    assert_eq!(
        watch.scan(),
        Default::default(),
        "the next report waits for the doubling"
    );
    release.wait();
    holder.join().expect("join holder");
    waiter.join().expect("join waiter");
    let stats = watch.stats();
    assert_eq!(
        (stats.contended, stats.long_waits, stats.deadlocks),
        (1, 1, 0)
    );
}

/// Two threads take two locks in opposite order and deadlock for good. The
/// threads and mutexes are leaked; the test process exits around them.
#[test]
fn a_lock_order_deadlock_is_reported_as_a_cycle() {
    let dir = std::env::temp_dir().join(format!("ternvale-log-{}-lockwatch", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let guard = crate::init(crate::LogConfig::new("lockwatch", dir.clone())).expect("log");
    let watch = LockWatch::manual(Duration::from_millis(40));
    let a: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
    let b: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
    let both_hold_one = Arc::new(Barrier::new(2));
    for (name, first, first_name, second, second_name) in [
        ("deadlock-ab", a, "lock-a", b, "lock-b"),
        ("deadlock-ba", b, "lock-b", a, "lock-a"),
    ] {
        let (watch, barrier) = (watch.clone(), Arc::clone(&both_hold_one));
        spawn_named(name, move || {
            let _first = watch.lock(first, first_name);
            barrier.wait();
            let _second = watch.lock(second, second_name);
        });
    }
    std::thread::sleep(Duration::from_millis(100));
    let scan = watch.scan();
    let mut waits: Vec<(String, String, String)> = scan
        .long_waits
        .iter()
        .map(|w| {
            let holder = w
                .holder
                .as_ref()
                .map(|h| h.thread.clone())
                .unwrap_or_default();
            (w.thread.clone(), w.lock.clone(), holder)
        })
        .collect();
    waits.sort();
    assert_eq!(
        waits,
        vec![
            ("deadlock-ab".into(), "lock-b".into(), "deadlock-ba".into()),
            ("deadlock-ba".into(), "lock-a".into(), "deadlock-ab".into()),
        ]
    );
    assert_eq!(scan.deadlocks.len(), 1, "{scan:?}");
    let Deadlock { threads, locks } = &scan.deadlocks[0];
    let ab = threads
        .iter()
        .position(|t| t == "deadlock-ab")
        .expect("ab in cycle");
    assert_eq!(threads.len(), 2);
    assert_eq!(locks[ab], "lock-b");
    assert_eq!(locks[1 - ab], "lock-a");
    let stats = watch.stats();
    assert_eq!((stats.long_waits, stats.deadlocks), (2, 1));
    assert_eq!(
        watch.scan(),
        Default::default(),
        "repeats only at the doubling"
    );

    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    for needle in [
        "lock wait exceeds threshold; possible deadlock",
        "holder=\"deadlock-ba\"",
        "deadlock-ab on lock-b for",
        "(held by deadlock-ba for",
        "ERROR",
        "deadlock: lock wait-for cycle",
        "waits lock-b held by deadlock-ba",
        "waits lock-a held by deadlock-ab",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
    }
    let sample = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/ternvale-log-samples/lockwatch-deadlock.log");
    if let Some(parent) = sample.parent() {
        std::fs::create_dir_all(parent).expect("sample dir");
    }
    std::fs::write(&sample, &text).expect("sample");
    std::fs::remove_dir_all(&dir).expect("remove log dir");
}

/// A thread locking a mutex it already holds deadlocks on itself. Leaked.
#[test]
fn relocking_a_held_mutex_is_a_cycle_of_one() {
    let watch = LockWatch::manual(Duration::from_millis(40));
    let mutex: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
    let thread_watch = watch.clone();
    spawn_named("relocker", move || {
        let _outer = thread_watch.lock(mutex, "guest-memory");
        let _inner = thread_watch.lock(mutex, "guest-memory");
    });
    std::thread::sleep(Duration::from_millis(100));
    let scan = watch.scan();
    assert_eq!(
        scan.deadlocks,
        vec![Deadlock {
            threads: vec!["relocker".into()],
            locks: vec!["guest-memory".into()],
        }]
    );
    let holder = scan.long_waits[0].holder.as_ref().expect("holder");
    assert_eq!(holder.thread, "relocker");
}

#[test]
fn the_monitor_reports_by_itself_and_exits_when_idle() {
    let watch = LockWatch::new(Duration::from_millis(30));
    let mutex = Arc::new(Mutex::new(()));
    let held = watch.lock(&mutex, "watched");
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
