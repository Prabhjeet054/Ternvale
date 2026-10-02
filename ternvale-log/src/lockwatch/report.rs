//! Turns a snapshot of every thread's holds and waits into reports: long
//! waits with their lock's holder, and wait-for cycles (deadlocks).

use std::sync::Arc;
use std::time::Duration;

use super::{Deadlock, Holder, LongWait, Scan, WaitKind};

pub(super) struct HeldView {
    pub lock: usize,
    pub name: Arc<str>,
    pub held: Duration,
}

pub(super) struct WaitView {
    pub lock: usize,
    pub name: Arc<str>,
    pub kind: WaitKind,
    pub waited: Duration,
    /// Reached its next report time in this scan.
    pub due: bool,
    /// Due for the first time.
    pub first: bool,
}

pub(super) struct ThreadView {
    pub thread: String,
    pub held: Vec<HeldView>,
    pub wait: Option<WaitView>,
}

pub(super) struct Analysis {
    pub scan: Scan,
    pub new_long_waits: u64,
    pub new_deadlocks: u64,
    /// One line per waiting thread, for context.
    pub waiting: Vec<String>,
}

pub(super) fn analyse(views: &[ThreadView]) -> Analysis {
    let mut long_waits = Vec::new();
    let mut new_long_waits = 0;
    for view in views {
        let Some(wait) = view.wait.as_ref().filter(|wait| wait.due) else {
            continue;
        };
        new_long_waits += u64::from(wait.first);
        long_waits.push(LongWait {
            lock: wait.name.to_string(),
            thread: view.thread.clone(),
            waited: wait.waited,
            kind: wait.kind,
            holder: lock_holder(views, wait).map(|(index, held)| Holder {
                thread: views[index].thread.clone(),
                held,
                holds: views[index]
                    .held
                    .iter()
                    .map(|held| held.name.to_string())
                    .collect(),
            }),
        });
    }
    let (deadlocks, new_deadlocks) = cycles(views);
    Analysis {
        scan: Scan {
            long_waits,
            deadlocks,
        },
        new_long_waits,
        new_deadlocks,
        waiting: views
            .iter()
            .filter_map(|view| describe(views, view))
            .collect(),
    }
}

/// The thread holding the lock a [`WaitKind::Lock`] wait is blocked on, and
/// for how long it has held it.
fn lock_holder(views: &[ThreadView], wait: &WaitView) -> Option<(usize, Duration)> {
    if wait.kind != WaitKind::Lock {
        return None;
    }
    views.iter().enumerate().find_map(|(index, view)| {
        view.held
            .iter()
            .find(|held| held.lock == wait.lock)
            .map(|held| (index, held.held))
    })
}

/// Cycles in the wait-for graph (thread -> holder of the lock it waits for)
/// with at least one wait due in this scan. A thread waits for at most one
/// lock and a lock has at most one holder, so following edges from each
/// thread finds every cycle.
fn cycles(views: &[ThreadView]) -> (Vec<Deadlock>, u64) {
    let next = |index: usize| {
        let wait = views[index].wait.as_ref()?;
        lock_holder(views, wait).map(|(holder, _)| holder)
    };
    let mut found: Vec<Vec<usize>> = Vec::new();
    for start in 0..views.len() {
        let mut path = vec![start];
        let mut current = start;
        while let Some(holder) = next(current) {
            if holder == start {
                let lowest = (0..path.len()).min_by_key(|&i| path[i]).unwrap_or(0);
                path.rotate_left(lowest);
                if !found.contains(&path) {
                    found.push(path);
                }
                break;
            }
            if path.contains(&holder) {
                break;
            }
            path.push(holder);
            current = holder;
        }
    }
    let mut deadlocks = Vec::new();
    let mut new = 0;
    for cycle in found {
        let waits: Vec<&WaitView> = cycle
            .iter()
            .filter_map(|&index| views[index].wait.as_ref())
            .collect();
        if !waits.iter().any(|wait| wait.due) {
            continue;
        }
        new += u64::from(waits.iter().any(|wait| wait.first));
        deadlocks.push(Deadlock {
            threads: cycle
                .iter()
                .map(|&index| views[index].thread.clone())
                .collect(),
            locks: waits.iter().map(|wait| wait.name.to_string()).collect(),
        });
    }
    (deadlocks, new)
}

fn describe(views: &[ThreadView], view: &ThreadView) -> Option<String> {
    let wait = view.wait.as_ref()?;
    let ms = wait.waited.as_millis();
    Some(match wait.kind {
        WaitKind::Lock => match lock_holder(views, wait) {
            Some((holder, held)) => format!(
                "{} on {} for {ms}ms (held by {} for {}ms)",
                view.thread,
                wait.name,
                views[holder].thread,
                held.as_millis()
            ),
            None => format!(
                "{} on {} for {ms}ms (holder unknown)",
                view.thread, wait.name
            ),
        },
        WaitKind::Condvar => format!(
            "{} waiting on {} condvar for {ms}ms",
            view.thread, wait.name
        ),
        WaitKind::Parked => format!("{} parked on {} for {ms}ms", view.thread, wait.name),
    })
}

/// "a waits lock-b held by b -> b waits lock-a held by a".
fn cycle_text(deadlock: &Deadlock) -> String {
    let count = deadlock.threads.len();
    (0..count)
        .map(|i| {
            format!(
                "{} waits {} held by {}",
                deadlock.threads[i],
                deadlock.locks[i],
                deadlock.threads[(i + 1) % count]
            )
        })
        .collect::<Vec<_>>()
        .join(" -> ")
}

pub(super) fn log(analysis: &Analysis, threshold: Duration) {
    let threshold_ms = threshold.as_millis() as u64;
    let waiting = &analysis.waiting;
    for wait in &analysis.scan.long_waits {
        let waited_ms = wait.waited.as_millis() as u64;
        if wait.kind != WaitKind::Lock {
            tracing::warn!(
                target: "ternvale::lock",
                lock = %wait.lock,
                thread = %wait.thread,
                waited_ms,
                threshold_ms,
                waiting = ?waiting,
                "condvar wait exceeds threshold"
            );
            continue;
        }
        let (holder, held_ms, holder_holds) = match &wait.holder {
            Some(holder) => (
                holder.thread.as_str(),
                holder.held.as_millis() as u64,
                holder.holds.as_slice(),
            ),
            None => ("unknown", 0, [].as_slice()),
        };
        tracing::warn!(
            target: "ternvale::lock",
            lock = %wait.lock,
            thread = %wait.thread,
            waited_ms,
            threshold_ms,
            holder,
            held_ms,
            holder_holds = ?holder_holds,
            waiting = ?waiting,
            "lock wait exceeds threshold; possible deadlock"
        );
    }
    for deadlock in &analysis.scan.deadlocks {
        tracing::error!(
            target: "ternvale::lock",
            cycle = %cycle_text(deadlock),
            threads = ?deadlock.threads,
            "deadlock: lock wait-for cycle"
        );
    }
}
