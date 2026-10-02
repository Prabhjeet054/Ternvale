//! Per-thread lock state: what each thread holds and what it waits for.
//!
//! Each thread owns one [`Slot`] per watch, found through a thread-local, so
//! recording a hold or a wait locks only that thread's own record. The watch
//! keeps weak references to every slot for the monitor's snapshot. Lock order
//! inside the watch: the slot list, then one slot's state.

use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use super::report::{HeldView, ThreadView, WaitView};
use super::{monitor, Inner, WaitKind};

struct Held {
    token: u64,
    lock: usize,
    name: Arc<str>,
    since: Instant,
}

struct Wait {
    lock: usize,
    name: Arc<str>,
    kind: WaitKind,
    since: Instant,
    next_report: Duration,
    reported: bool,
}

#[derive(Default)]
struct SlotState {
    held: Vec<Held>,
    wait: Option<Wait>,
    next_token: u64,
}

/// One thread's holds and current wait in one watch.
pub(super) struct Slot {
    thread: String,
    inner: Arc<Inner>,
    state: Mutex<SlotState>,
}

thread_local! {
    static SLOTS: RefCell<Vec<Arc<Slot>>> = const { RefCell::new(Vec::new()) };
}

/// The calling thread's slot in `inner`, registered on first use. If the
/// thread-local is already gone (thread teardown), the slot is untracked.
pub(super) fn slot(inner: &Arc<Inner>) -> Arc<Slot> {
    let found = SLOTS.try_with(|slots| {
        let mut slots = slots.borrow_mut();
        if let Some(slot) = slots.iter().find(|slot| Arc::ptr_eq(&slot.inner, inner)) {
            return Arc::clone(slot);
        }
        let slot = Slot::new(inner);
        let mut list = inner.slots.lock().unwrap_or_else(PoisonError::into_inner);
        list.retain(|weak| weak.strong_count() > 0);
        list.push(Arc::downgrade(&slot));
        drop(list);
        slots.push(Arc::clone(&slot));
        slot
    });
    found.unwrap_or_else(|_| Slot::new(inner))
}

/// Every live thread's holds and wait. With `mark`, a wait that reached its
/// next report time is flagged `due` and its next report moves to twice the
/// time waited.
pub(super) fn snapshot(inner: &Inner, now: Instant, mark: bool) -> Vec<ThreadView> {
    let slots: Vec<Arc<Slot>> = inner
        .slots
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
    slots
        .iter()
        .map(|slot| {
            let mut state = slot.state();
            let held = state
                .held
                .iter()
                .map(|held| HeldView {
                    lock: held.lock,
                    name: Arc::clone(&held.name),
                    held: now.saturating_duration_since(held.since),
                })
                .collect();
            let wait = state.wait.as_mut().map(|wait| {
                let waited = now.saturating_duration_since(wait.since);
                let due = mark && wait.kind != WaitKind::Parked && waited >= wait.next_report;
                let first = due && !wait.reported;
                if due {
                    wait.reported = true;
                    wait.next_report = waited * 2;
                }
                WaitView {
                    lock: wait.lock,
                    name: Arc::clone(&wait.name),
                    kind: wait.kind,
                    waited,
                    due,
                    first,
                }
            });
            ThreadView {
                thread: slot.thread.clone(),
                held,
                wait,
            }
        })
        .collect()
}

impl Slot {
    fn new(inner: &Arc<Inner>) -> Arc<Self> {
        let current = std::thread::current();
        let thread = match current.name() {
            Some(name) => name.to_string(),
            None => format!("{:?}", current.id()),
        };
        Arc::new(Self {
            thread,
            inner: Arc::clone(inner),
            state: Mutex::new(SlotState::default()),
        })
    }

    fn state(&self) -> MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record that this thread now holds `lock`. Returns the release token.
    pub(super) fn hold(&self, lock: usize, name: &Arc<str>) -> u64 {
        let mut state = self.state();
        let token = state.next_token;
        state.next_token += 1;
        state.held.push(Held {
            token,
            lock,
            name: Arc::clone(name),
            since: Instant::now(),
        });
        token
    }

    pub(super) fn release(&self, token: u64) {
        let mut state = self.state();
        if let Some(index) = state.held.iter().rposition(|held| held.token == token) {
            state.held.remove(index);
        }
    }

    pub(super) fn begin_wait(&self, lock: usize, name: &Arc<str>, kind: WaitKind, since: Instant) {
        self.state().wait = Some(Wait {
            lock,
            name: Arc::clone(name),
            kind,
            since,
            next_report: self.inner.threshold,
            reported: false,
        });
        if kind != WaitKind::Parked {
            self.inner.active.fetch_add(1, Ordering::AcqRel);
            monitor::ensure(&self.inner);
        }
    }

    pub(super) fn finish_wait(&self, name: &str, kind: WaitKind, waited: Duration) {
        let reported = self.state().wait.take().is_some_and(|wait| wait.reported);
        let inner = &self.inner;
        if kind != WaitKind::Parked {
            inner.active.fetch_sub(1, Ordering::AcqRel);
        }
        let micros = u64::try_from(waited.as_micros()).unwrap_or(u64::MAX);
        let long = waited >= inner.threshold;
        match kind {
            WaitKind::Lock => {
                inner.contended.fetch_add(1, Ordering::Relaxed);
                inner.max_wait_us.fetch_max(micros, Ordering::Relaxed);
                if !long {
                    tracing::trace!(target: "ternvale::lock", lock = name, waited_us = micros, "contended lock acquired");
                    return;
                }
            }
            WaitKind::Condvar if !long => return,
            WaitKind::Condvar => {}
            WaitKind::Parked => {
                tracing::debug!(target: "ternvale::lock", lock = name, waited_ms = waited.as_millis() as u64, "parked wait ended");
                return;
            }
        }
        if !reported {
            inner.long_waits.fetch_add(1, Ordering::Relaxed);
        }
        let waited_ms = waited.as_millis() as u64;
        if kind == WaitKind::Lock {
            tracing::warn!(target: "ternvale::lock", lock = name, waited_ms, "lock acquired after a long wait");
        } else {
            tracing::warn!(target: "ternvale::lock", lock = name, waited_ms, "condvar wait ended after a long wait");
        }
    }
}
