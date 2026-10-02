//! The monitor thread: scans every quarter threshold while any lock or brief
//! condvar wait is in progress, and exits after two idle thresholds. The next
//! such wait starts a new one.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{Inner, LockWatch};

/// Start the monitor unless it is running (or the watch is manual).
pub(super) fn ensure(inner: &Arc<Inner>) {
    if !inner.auto_monitor || !claim(inner) {
        return;
    }
    let watch = LockWatch {
        inner: Arc::clone(inner),
    };
    let dispatch = tracing::dispatcher::get_default(|current| current.clone());
    let spawned = std::thread::Builder::new()
        .name("lockwatch".to_string())
        .spawn(move || tracing::dispatcher::with_default(&dispatch, || run(&watch)));
    if let Err(error) = spawned {
        inner.monitor.store(false, Ordering::Release);
        tracing::warn!(target: "ternvale::lock", error = %error, "could not start the lock watch monitor");
    }
}

fn claim(inner: &Inner) -> bool {
    inner
        .monitor
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

fn run(watch: &LockWatch) {
    let inner = &watch.inner;
    let threshold = inner.threshold;
    let tick = (threshold / 4).max(Duration::from_millis(1));
    tracing::debug!(target: "ternvale::lock", threshold_ms = threshold.as_millis() as u64, "lock watch monitor started");
    let mut idle_since: Option<Instant> = None;
    loop {
        std::thread::sleep(tick);
        watch.scan();
        if inner.active.load(Ordering::Acquire) > 0 {
            idle_since = None;
            continue;
        }
        let idle = *idle_since.get_or_insert_with(Instant::now);
        if idle.elapsed() < threshold * 2 {
            continue;
        }
        inner.monitor.store(false, Ordering::Release);
        // A wait that began after the check above saw the monitor still
        // running and did not start one; keep going for it.
        if inner.active.load(Ordering::Acquire) > 0 && claim(inner) {
            idle_since = None;
            continue;
        }
        tracing::debug!(target: "ternvale::lock", "lock watch monitor idle; exiting");
        return;
    }
}
