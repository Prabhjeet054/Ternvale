//! Lock watch: the deadlock detector.
//!
//! [`lock`] takes a `std::sync::Mutex` like `Mutex::lock` and returns a
//! [`Guard`] that records which thread holds the lock. A poisoned lock is
//! recovered with a WARN. Each thread keeps its own list of held locks and its
//! current wait, so an uncontended lock costs a `try_lock` plus an update of
//! the calling thread's own (uncontended) record.
//!
//! A monitor thread starts on the first contended wait. Every quarter
//! threshold it logs, at WARN on `ternvale::lock`, each wait longer than
//! [`LONG_WAIT`] *while it is still waiting*, naming the thread that holds the
//! lock and listing every other waiting thread. When the waits form a cycle
//! (each thread waits for a lock the next one holds, including a thread
//! re-locking a mutex it already holds) the cycle is logged at ERROR.
//!
//! Condvar waits go through the guard: [`Guard::wait`] and
//! [`Guard::wait_timeout`] are expected to be short and are reported like lock
//! waits. [`Guard::park`] may last indefinitely (a powered-off CPU waiting for
//! `CPU_ON`); it is listed as context in reports but never reported itself.

mod monitor;
mod registry;
mod report;

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError, Weak};
use std::time::{Duration, Instant};

use registry::Slot;

/// A lock or brief condvar wait longer than this is logged at WARN.
pub const LONG_WAIT: Duration = Duration::from_secs(2);

/// What a waiting thread is blocked in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitKind {
    /// `Mutex::lock` on a contended mutex.
    Lock,
    /// A condvar wait expected to be short ([`Guard::wait`], [`Guard::wait_timeout`]).
    Condvar,
    /// A condvar wait that may last indefinitely ([`Guard::park`]).
    Parked,
}

/// The thread holding a lock that another thread waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// Name (or id) of the holding thread.
    pub thread: String,
    /// How long it had held the lock when reported.
    pub held: Duration,
    /// Every lock the holder holds, in acquisition order.
    pub holds: Vec<String>,
}

/// One wait that crossed the threshold, as reported by [`LockWatch::scan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongWait {
    /// Name passed to [`lock`].
    pub lock: String,
    /// Name (or id) of the waiting thread.
    pub thread: String,
    /// How long it had waited when reported.
    pub waited: Duration,
    /// Lock or condvar wait.
    pub kind: WaitKind,
    /// Holder of the lock, for a [`WaitKind::Lock`] wait whose holder is known.
    pub holder: Option<Holder>,
}

/// A wait-for cycle: `threads[i]` waits for `locks[i]`, which
/// `threads[(i + 1) % n]` holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deadlock {
    /// Threads in the cycle.
    pub threads: Vec<String>,
    /// The lock each thread waits for.
    pub locks: Vec<String>,
}

/// Everything one [`LockWatch::scan`] reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// Waits that crossed the threshold, or its next doubling.
    pub long_waits: Vec<LongWait>,
    /// Wait-for cycles with at least one wait in `long_waits`.
    pub deadlocks: Vec<Deadlock>,
}

/// Counters since the watch was created.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LockStats {
    /// Lock acquisitions that had to block.
    pub contended: u64,
    /// Lock and brief condvar waits that crossed the threshold.
    pub long_waits: u64,
    /// Longest completed lock wait, in microseconds.
    pub max_wait_us: u64,
    /// Distinct wait-for cycles found.
    pub deadlocks: u64,
}

struct Inner {
    threshold: Duration,
    auto_monitor: bool,
    slots: Mutex<Vec<Weak<Slot>>>,
    /// Lock and brief condvar waits in progress (not parked ones).
    active: AtomicUsize,
    monitor: AtomicBool,
    contended: AtomicU64,
    long_waits: AtomicU64,
    max_wait_us: AtomicU64,
    deadlocks: AtomicU64,
}

/// Registry of every thread's held locks and waits, plus the monitor that
/// reports long waits and deadlocks.
#[derive(Clone)]
pub struct LockWatch {
    inner: Arc<Inner>,
}

/// Lock `mutex` through the process-wide [`LockWatch`] (threshold [`LONG_WAIT`]).
#[tracing::instrument(level = "debug", target = "ternvale::lock", skip_all, fields(lock = name))]
pub fn lock<'a, T: ?Sized>(mutex: &'a Mutex<T>, name: &str) -> Guard<'a, T> {
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
                slots: Mutex::new(Vec::new()),
                active: AtomicUsize::new(0),
                monitor: AtomicBool::new(false),
                contended: AtomicU64::new(0),
                long_waits: AtomicU64::new(0),
                max_wait_us: AtomicU64::new(0),
                deadlocks: AtomicU64::new(0),
            }),
        }
    }

    /// Like `Mutex::lock`, but the holder is recorded, a contended wait is
    /// watched, and a poisoned lock is recovered with a WARN.
    #[tracing::instrument(level = "debug", target = "ternvale::lock", skip_all, fields(lock = name))]
    pub fn lock<'a, T: ?Sized>(&self, mutex: &'a Mutex<T>, name: &str) -> Guard<'a, T> {
        let slot = registry::slot(&self.inner);
        let id = mutex as *const Mutex<T> as *const () as usize;
        let name: Arc<str> = Arc::from(name);
        let guard = match mutex.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poison)) => recover(poison.into_inner(), &name),
            Err(TryLockError::WouldBlock) => {
                let started = Instant::now();
                slot.begin_wait(id, &name, WaitKind::Lock, started);
                let guard = mutex
                    .lock()
                    .unwrap_or_else(|poison| recover(poison.into_inner(), &name));
                slot.finish_wait(&name, WaitKind::Lock, started.elapsed());
                guard
            }
        };
        Guard::new(slot, id, name, guard)
    }

    /// Log and return every wait that crossed the threshold, or its next
    /// doubling, since the last scan, and every wait-for cycle among them.
    /// The monitor calls this every quarter threshold.
    #[tracing::instrument(level = "debug", target = "ternvale::lock", skip_all)]
    pub fn scan(&self) -> Scan {
        let views = registry::snapshot(&self.inner, Instant::now(), true);
        let analysis = report::analyse(&views);
        let inner = &self.inner;
        inner
            .long_waits
            .fetch_add(analysis.new_long_waits, Ordering::Relaxed);
        inner
            .deadlocks
            .fetch_add(analysis.new_deadlocks, Ordering::Relaxed);
        report::log(&analysis, inner.threshold);
        analysis.scan
    }

    /// Counters since this watch was created.
    #[tracing::instrument(level = "debug", target = "ternvale::lock", skip_all)]
    pub fn stats(&self) -> LockStats {
        let inner = &self.inner;
        LockStats {
            contended: inner.contended.load(Ordering::Relaxed),
            long_waits: inner.long_waits.load(Ordering::Relaxed),
            max_wait_us: inner.max_wait_us.load(Ordering::Relaxed),
            deadlocks: inner.deadlocks.load(Ordering::Relaxed),
        }
    }
}

/// A held lock. Dereferences to the data; dropping it unlocks the mutex.
#[must_use = "dropping the guard unlocks the mutex"]
pub struct Guard<'a, T: ?Sized> {
    // Declared first so the hold record is removed before the mutex unlocks.
    hold: Hold,
    inner: MutexGuard<'a, T>,
}

struct Hold {
    slot: Arc<Slot>,
    token: u64,
    lock: usize,
    name: Arc<str>,
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.slot.release(self.token);
    }
}

type Waited<'a, T, R> = Result<(MutexGuard<'a, T>, R), PoisonError<(MutexGuard<'a, T>, R)>>;

impl<'a, T: ?Sized> Guard<'a, T> {
    fn new(slot: Arc<Slot>, lock: usize, name: Arc<str>, inner: MutexGuard<'a, T>) -> Self {
        let token = slot.hold(lock, &name);
        Self {
            hold: Hold {
                slot,
                token,
                lock,
                name,
            },
            inner,
        }
    }
}

impl<'a, T> Guard<'a, T> {
    /// `Condvar::wait`, expected to be short: reported if it passes the threshold.
    #[tracing::instrument(level = "debug", target = "ternvale::lock", skip_all, fields(lock = %self.hold.name))]
    pub fn wait(self, condvar: &Condvar) -> Self {
        self.wait_as(WaitKind::Condvar, |guard| match condvar.wait(guard) {
            Ok(guard) => Ok((guard, ())),
            Err(poison) => Err(PoisonError::new((poison.into_inner(), ()))),
        })
        .0
    }

    /// `Condvar::wait` that may last indefinitely. Listed as context in
    /// reports, never reported itself.
    #[tracing::instrument(level = "debug", target = "ternvale::lock", skip_all, fields(lock = %self.hold.name))]
    pub fn park(self, condvar: &Condvar) -> Self {
        self.wait_as(WaitKind::Parked, |guard| match condvar.wait(guard) {
            Ok(guard) => Ok((guard, ())),
            Err(poison) => Err(PoisonError::new((poison.into_inner(), ()))),
        })
        .0
    }

    /// `Condvar::wait_timeout`, treated like [`Guard::wait`]. The flag is true
    /// when the timeout elapsed.
    #[tracing::instrument(level = "debug", target = "ternvale::lock", skip_all, fields(lock = %self.hold.name))]
    pub fn wait_timeout(self, condvar: &Condvar, timeout: Duration) -> (Self, bool) {
        self.wait_as(WaitKind::Condvar, |guard| {
            match condvar.wait_timeout(guard, timeout) {
                Ok((guard, result)) => Ok((guard, result.timed_out())),
                Err(poison) => {
                    let (guard, result) = poison.into_inner();
                    Err(PoisonError::new((guard, result.timed_out())))
                }
            }
        })
    }

    fn wait_as<R>(
        self,
        kind: WaitKind,
        block: impl FnOnce(MutexGuard<'a, T>) -> Waited<'a, T, R>,
    ) -> (Self, R) {
        let Guard { hold, inner } = self;
        let (slot, lock, name) = (Arc::clone(&hold.slot), hold.lock, Arc::clone(&hold.name));
        drop(hold);
        let started = Instant::now();
        slot.begin_wait(lock, &name, kind, started);
        let (inner, result) = block(inner).unwrap_or_else(|poison| {
            tracing::warn!(target: "ternvale::lock", lock = %name, "lock was poisoned by a panic during a condvar wait; continuing");
            poison.into_inner()
        });
        slot.finish_wait(&name, kind, started.elapsed());
        (Guard::new(slot, lock, name, inner), result)
    }
}

impl<T: ?Sized> Deref for Guard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T: ?Sized> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
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
        self.inner.monitor.load(Ordering::Acquire)
    }

    /// Names of the locks `thread` holds, in acquisition order.
    pub(crate) fn held_by(&self, thread: &str) -> Vec<String> {
        registry::snapshot(&self.inner, Instant::now(), false)
            .into_iter()
            .filter(|view| view.thread == thread)
            .flat_map(|view| view.held.into_iter().map(|held| held.name.to_string()))
            .collect()
    }

    /// Waits in progress, as (thread, lock, kind).
    pub(crate) fn waits(&self) -> Vec<(String, String, WaitKind)> {
        registry::snapshot(&self.inner, Instant::now(), false)
            .into_iter()
            .filter_map(|view| {
                let wait = view.wait?;
                Some((view.thread, wait.name.to_string(), wait.kind))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod condvar_tests;
