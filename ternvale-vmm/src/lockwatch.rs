//! Lock-wait watchdog: the deadlock detector.
//!
//! [`lock`] takes a `std::sync::Mutex` like `Mutex::lock`, recovering a
//! poisoned lock with a WARN. An uncontended lock costs one `try_lock`. A
//! contended wait is registered with the process-wide [`LockWatch`], and a
//! monitor thread logs every wait longer than [`LONG_WAIT`] at WARN on
//! `ternvale::lock` *while it is still waiting*, together with every other
//! thread that is waiting. A real deadlock is therefore reported even though
//! the stuck threads never return.
//!
//! Condvar waits are deliberate (a powered-off CPU waits for `CPU_ON`) and are
//! not watched. The detector sees waiters, not holders: std mutexes do not say
//! who holds them.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError};
use std::time::{Duration, Instant};

/// A lock wait longer than this is logged at WARN.
pub const LONG_WAIT: Duration = Duration::from_secs(2);

/// One wait that crossed the threshold, as reported by [`LockWatch::scan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongWait {
    /// Name passed to [`lock`].
    pub lock: String,
    /// Name (or id) of the waiting thread.
    pub thread: String,
    /// How long it had waited when reported.
    pub waited: Duration,
}

/// Counters since the watch was created.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LockStats {
    /// Acquisitions that had to block.
    pub contended: u64,
    /// Waits that crossed the threshold.
    pub long_waits: u64,
    /// Longest completed wait, in microseconds.
    pub max_wait_us: u64,
}

struct Wait {
    lock: String,
    thread: String,
    since: Instant,
    next_report: Duration,
    reported: bool,
}

#[derive(Default)]
struct State {
    waits: BTreeMap<u64, Wait>,
    next_token: u64,
    monitor: bool,
    idle_since: Option<Instant>,
}

struct Inner {
    threshold: Duration,
    auto_monitor: bool,
    state: Mutex<State>,
    contended: AtomicU64,
    long_waits: AtomicU64,
    max_wait_us: AtomicU64,
}

/// Registry of contended lock waits plus the monitor that reports long ones.
#[derive(Clone)]
pub struct LockWatch {
    inner: Arc<Inner>,
}

/// Lock `mutex` through the process-wide [`LockWatch`] (threshold [`LONG_WAIT`]).
#[tracing::instrument(level = "debug", target = "ternvale::lock", skip_all, fields(lock = name))]
pub fn lock<'a, T: ?Sized>(mutex: &'a Mutex<T>, name: &str) -> MutexGuard<'a, T> {
    LockWatch::global().lock(mutex, name)
}

impl LockWatch {
    /// A watch whose monitor thread starts on the first contended wait.
    #[tracing::instrument(level = "debug", target = "ternvale::lock", skip_all, fields(threshold_ms = threshold.as_millis() as u64))]
    pub fn new(threshold: Duration) -> Self {
        Self::build(threshold, true)
    }

    /// The process-wide watch used by [`lock`].
    #[tracing::instrument(level = "debug", target = "ternvale::lock", skip_all)]
    pub fn global() -> &'static LockWatch {
        static GLOBAL: OnceLock<LockWatch> = OnceLock::new();
        GLOBAL.get_or_init(|| LockWatch::new(LONG_WAIT))
    }

    fn build(threshold: Duration, auto_monitor: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                threshold,
                auto_monitor,
                state: Mutex::new(State::default()),
                contended: AtomicU64::new(0),
                long_waits: AtomicU64::new(0),
                max_wait_us: AtomicU64::new(0),
            }),
        }
    }

    /// Like `Mutex::lock`, but a contended wait is watched and a poisoned lock
    /// is recovered with a WARN.
    #[tracing::instrument(level = "debug", target = "ternvale::lock", skip_all, fields(lock = name))]
    pub fn lock<'a, T: ?Sized>(&self, mutex: &'a Mutex<T>, name: &str) -> MutexGuard<'a, T> {
        match mutex.try_lock() {
            Ok(guard) => return guard,
            Err(TryLockError::Poisoned(poison)) => return recover(poison.into_inner(), name),
            Err(TryLockError::WouldBlock) => {}
        }
        let started = Instant::now();
        let token = self.begin(name, started);
        let guard = match mutex.lock() {
            Ok(guard) => guard,
            Err(poison) => recover(poison.into_inner(), name),
        };
        self.end(token, name, started.elapsed());
        guard
    }

    /// Log (WARN) and return every wait that crossed the threshold, or its
    /// next doubling, since the last scan. The monitor calls this every
    /// quarter threshold.
    #[tracing::instrument(level = "debug", target = "ternvale::lock", skip_all)]
    pub fn scan(&self) -> Vec<LongWait> {
        let now = Instant::now();
        let mut reports = Vec::new();
        let mut first_reports = 0;
        let mut state = self.state();
        for wait in state.waits.values_mut() {
            let waited = now.saturating_duration_since(wait.since);
            if waited < wait.next_report {
                continue;
            }
            if !wait.reported {
                wait.reported = true;
                first_reports += 1;
            }
            wait.next_report = waited * 2;
            reports.push(LongWait {
                lock: wait.lock.clone(),
                thread: wait.thread.clone(),
                waited,
            });
        }
        let waiting: Vec<String> = state
            .waits
            .values()
            .map(|wait| {
                let ms = now.saturating_duration_since(wait.since).as_millis();
                format!("{} on {} for {ms}ms", wait.thread, wait.lock)
            })
            .collect();
        drop(state);
        self.inner
            .long_waits
            .fetch_add(first_reports, Ordering::Relaxed);
        for report in &reports {
            tracing::warn!(
                target: "ternvale::lock",
                lock = %report.lock,
                thread = %report.thread,
                waited_ms = report.waited.as_millis() as u64,
                threshold_ms = self.inner.threshold.as_millis() as u64,
                waiting = ?waiting,
                "lock wait exceeds threshold; possible deadlock"
            );
        }
        reports
    }

    /// Counters since this watch was created.
    #[tracing::instrument(level = "debug", target = "ternvale::lock", skip_all)]
    pub fn stats(&self) -> LockStats {
        LockStats {
            contended: self.inner.contended.load(Ordering::Relaxed),
            long_waits: self.inner.long_waits.load(Ordering::Relaxed),
            max_wait_us: self.inner.max_wait_us.load(Ordering::Relaxed),
        }
    }

    fn begin(&self, name: &str, since: Instant) -> u64 {
        let thread = std::thread::current();
        let thread = match thread.name() {
            Some(name) => name.to_string(),
            None => format!("{:?}", thread.id()),
        };
        let mut state = self.state();
        let token = state.next_token;
        state.next_token += 1;
        state.waits.insert(
            token,
            Wait {
                lock: name.to_string(),
                thread,
                since,
                next_report: self.inner.threshold,
                reported: false,
            },
        );
        state.idle_since = None;
        let start_monitor = self.inner.auto_monitor && !state.monitor;
        state.monitor |= start_monitor;
        drop(state);
        if start_monitor {
            self.spawn_monitor();
        }
        token
    }

    fn end(&self, token: u64, name: &str, waited: Duration) {
        let wait = self.state().waits.remove(&token);
        let micros = u64::try_from(waited.as_micros()).unwrap_or(u64::MAX);
        self.inner.contended.fetch_add(1, Ordering::Relaxed);
        self.inner.max_wait_us.fetch_max(micros, Ordering::Relaxed);
        if waited < self.inner.threshold {
            tracing::trace!(target: "ternvale::lock", lock = name, waited_us = micros, "contended lock acquired");
            return;
        }
        if !wait.is_some_and(|wait| wait.reported) {
            self.inner.long_waits.fetch_add(1, Ordering::Relaxed);
        }
        tracing::warn!(
            target: "ternvale::lock",
            lock = name,
            waited_ms = waited.as_millis() as u64,
            "lock acquired after a long wait"
        );
    }

    fn spawn_monitor(&self) {
        let watch = self.clone();
        let dispatch = tracing::dispatcher::get_default(|current| current.clone());
        let spawned = std::thread::Builder::new()
            .name("lockwatch".to_string())
            .spawn(move || tracing::dispatcher::with_default(&dispatch, || watch.monitor()));
        if let Err(error) = spawned {
            self.state().monitor = false;
            tracing::warn!(target: "ternvale::lock", error = %error, "could not start the lock watch monitor");
        }
    }

    /// Scan every quarter threshold; exit after two idle thresholds. The next
    /// contended wait starts a new monitor.
    fn monitor(&self) {
        let threshold = self.inner.threshold;
        let tick = (threshold / 4).max(Duration::from_millis(1));
        tracing::debug!(target: "ternvale::lock", threshold_ms = threshold.as_millis() as u64, "lock watch monitor started");
        loop {
            std::thread::sleep(tick);
            self.scan();
            let mut state = self.state();
            if !state.waits.is_empty() {
                state.idle_since = None;
                continue;
            }
            let idle = *state.idle_since.get_or_insert_with(Instant::now);
            if idle.elapsed() >= threshold * 2 {
                state.monitor = false;
                state.idle_since = None;
                drop(state);
                tracing::debug!(target: "ternvale::lock", "lock watch monitor idle; exiting");
                return;
            }
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn recover<'a, T: ?Sized>(guard: MutexGuard<'a, T>, name: &str) -> MutexGuard<'a, T> {
    tracing::warn!(target: "ternvale::lock", lock = name, "lock was poisoned by a panic; continuing");
    guard
}

#[cfg(test)]
impl LockWatch {
    /// A watch with no monitor thread, so tests drive [`LockWatch::scan`].
    pub(crate) fn manual(threshold: Duration) -> Self {
        Self::build(threshold, false)
    }

    pub(crate) fn monitor_running(&self) -> bool {
        self.state().monitor
    }
}

#[cfg(test)]
#[path = "lockwatch_tests.rs"]
mod tests;
