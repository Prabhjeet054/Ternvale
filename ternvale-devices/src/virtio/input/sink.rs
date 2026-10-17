//! Host-side event injection for one virtio-input device.
//!
//! [`InputSink`] is called from host threads (the display presenter), never
//! the vCPU. A report (events plus `SYN_REPORT`) is queued all-or-nothing and
//! written into the guest's eventq buffers right away. The rest waits in a
//! bounded backlog, which the next eventq kick flushes. When the backlog is
//! full or the driver has not bound the eventq, the whole report is dropped,
//! except a release of a key or button whose press was queued: dropping it
//! would leave the key held in the guest (and Linux turns its next press into
//! an autorepeat, which X ignores).
//! Lock order: device state, then guest memory.

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError, Weak};
use std::time::{Duration, Instant};

use ternvale_vmm::GuestMemory;

use super::super::irq::VirtioIrq;
use super::super::queue::SplitQueue;
use super::super::QueueNotify;
use super::codes::{self, InputEvent, ABS_MAX_VALUE, ABS_X, ABS_Y, REL_HWHEEL, REL_WHEEL};
use super::config::InputKind;

/// Most events held for the guest before reports are dropped. Releases of held
/// keys may exceed it by two events (release and `SYN_REPORT`) per held key.
pub const MAX_PENDING: usize = 64;

const INT_VRING: u32 = 1;
const DROP_LOG_INTERVAL: Duration = Duration::from_secs(1);

pub(super) type QueueKey = (u16, u64, u64, u64);

/// Counters for one virtio-input device.
#[derive(Debug, Default)]
pub struct InputStats {
    /// Events written to the guest.
    pub injected: AtomicU64,
    /// Events dropped (backlog full, driver not ready, or unsupported).
    pub dropped: AtomicU64,
    /// Events the guest sent on the status queue (LEDs).
    pub status: AtomicU64,
}

impl InputStats {
    /// `(injected, dropped, status)`.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::input", skip_all)]
    pub fn snapshot(&self) -> [u64; 3] {
        [
            self.injected.load(Ordering::Relaxed),
            self.dropped.load(Ordering::Relaxed),
            self.status.load(Ordering::Relaxed),
        ]
    }
}

#[derive(Default)]
struct DropLog {
    last: Option<Instant>,
    suppressed: u64,
}

#[derive(Default)]
pub(super) struct State {
    pub(super) eventq: Option<(QueueKey, SplitQueue)>,
    pending: VecDeque<InputEvent>,
    /// Keys and buttons whose press was queued and release was not.
    held: BTreeSet<u16>,
    drops: DropLog,
}

impl State {
    /// Every event is a release of a held key (`SYN_REPORT` aside).
    fn releases_held_keys(&self, report: &[InputEvent]) -> bool {
        let mut keys = report
            .iter()
            .filter(|e| e.ev_type != codes::EV_SYN)
            .peekable();
        keys.peek().is_some()
            && keys
                .all(|e| e.ev_type == codes::EV_KEY && e.value == 0 && self.held.contains(&e.code))
    }

    fn track_keys(&mut self, report: &[InputEvent]) {
        for event in report.iter().filter(|e| e.ev_type == codes::EV_KEY) {
            if event.value == 0 {
                self.held.remove(&event.code);
            } else {
                self.held.insert(event.code);
            }
        }
    }
}

pub(super) struct Shared {
    pub(super) kind: InputKind,
    pub(super) memory: Arc<Mutex<GuestMemory>>,
    pub(super) irq: Arc<VirtioIrq>,
    pub(super) stats: Arc<InputStats>,
    pub(super) state: Mutex<State>,
    /// An eventq kick not yet served, and its queue addresses.
    pub(super) kick: AtomicBool,
    pub(super) kicked: Mutex<Option<QueueNotify>>,
}

impl Shared {
    pub(super) fn lock_state(&self) -> ternvale_vmm::lockwatch::Guard<'_, State> {
        ternvale_vmm::lockwatch::lock(&self.state, "virtio-input")
    }

    /// The driver kicked eventq (vCPU thread). Never waits for the state lock,
    /// which a host thread may hold while it waits for guest memory: the
    /// holder serves the kick when it unlocks ([`Shared::serve_kicks`]).
    pub(super) fn kick_eventq(&self, notify: QueueNotify) {
        *ternvale_vmm::lockwatch::lock(&self.kicked, "virtio-input-kick") = Some(notify);
        self.kick.store(true, Ordering::SeqCst);
        self.serve_kicks();
    }

    /// Bind and flush for a pending kick, unless another thread holds the
    /// state lock. Every holder calls this after unlocking.
    pub(super) fn serve_kicks(&self) {
        fence(Ordering::SeqCst);
        while self.kick.load(Ordering::SeqCst) {
            let mut state = match self.state.try_lock() {
                Ok(state) => state,
                Err(TryLockError::WouldBlock) => {
                    tracing::trace!(
                        target: "ternvale::virtio::input",
                        device = self.kind.short(),
                        "virtio-input eventq kick left to the state lock holder"
                    );
                    return;
                }
                Err(TryLockError::Poisoned(poisoned)) => {
                    tracing::warn!(target: "ternvale::virtio::input", device = self.kind.short(), "virtio-input state lock was poisoned; continuing");
                    poisoned.into_inner()
                }
            };
            if !self.kick.swap(false, Ordering::SeqCst) {
                return;
            }
            let notify = ternvale_vmm::lockwatch::lock(&self.kicked, "virtio-input-kick").take();
            if let Some(notify) = notify {
                super::VirtioInput::bind(&mut state.eventq, &notify, self.kind.short());
            }
            self.flush(&mut state);
            drop(state);
            fence(Ordering::SeqCst);
        }
    }

    /// Discard the queue binding and backlog (device reset).
    pub(super) fn reset(&self) {
        let mut state = self.lock_state();
        let pending = state.pending.len();
        self.kick.store(false, Ordering::SeqCst);
        ternvale_vmm::lockwatch::lock(&self.kicked, "virtio-input-kick").take();
        state.eventq = None;
        state.pending.clear();
        state.held.clear();
        tracing::debug!(
            target: "ternvale::virtio::input",
            device = self.kind.short(),
            discarded = pending,
            "virtio-input eventq reset"
        );
    }

    /// Write backlog events into available eventq buffers.
    pub(super) fn flush(&self, state: &mut State) {
        let State {
            eventq, pending, ..
        } = state;
        let Some((_, queue)) = eventq.as_mut() else {
            return;
        };
        let mut completed = false;
        // Logged after guest memory is unlocked: other devices wait for it.
        let trace = tracing::enabled!(target: "ternvale::virtio::input", tracing::Level::TRACE);
        let mut injected = Vec::new();
        let mut mem = ternvale_vmm::lockwatch::lock(&self.memory, "guest-memory");
        while let Some(event) = pending.front().copied() {
            let next = queue.chains(&mem).next();
            let Some(chain) = next else {
                break;
            };
            let target = chain.writable.first().filter(|b| b.len >= 8);
            let written = match target {
                Some(buf) if chain.readable.is_empty() => {
                    mem.write_bytes(buf.addr, &event.to_bytes()).is_ok()
                }
                _ => false,
            };
            if !written {
                tracing::warn!(
                    target: "ternvale::virtio::input",
                    device = self.kind.short(),
                    head = chain.head,
                    "virtio-input event buffer is not one writable 8-byte buffer in guest memory; completing empty"
                );
                completed |= queue.add_used(&mut mem, chain.head, 0);
                continue;
            }
            pending.pop_front();
            completed |= queue.add_used(&mut mem, chain.head, 8);
            self.stats.injected.fetch_add(1, Ordering::Relaxed);
            if trace {
                injected.push((event, chain.head));
            }
        }
        let notify = completed && queue.notification_needed(&mem);
        drop(mem);
        if notify {
            self.irq.raise(INT_VRING);
        }
        for (event, head) in injected {
            tracing::trace!(
                target: "ternvale::virtio::input",
                device = self.kind.short(),
                ev_type = codes::type_name(event.ev_type),
                code = %format!("{:#x}", event.code),
                value = event.value as i32,
                head,
                "virtio-input event injected"
            );
        }
    }

    fn drop_report(&self, state: &mut State, events: &[InputEvent], reason: &'static str) {
        let count = events.len() as u64;
        self.stats.dropped.fetch_add(count, Ordering::Relaxed);
        let now = Instant::now();
        let due = state
            .drops
            .last
            .is_none_or(|at| now.duration_since(at) >= DROP_LOG_INTERVAL);
        if !due {
            state.drops.suppressed += count;
            tracing::trace!(
                target: "ternvale::virtio::input",
                device = self.kind.short(),
                reason,
                events = count,
                "virtio-input report dropped (warning rate-limited)"
            );
            return;
        }
        let suppressed = std::mem::take(&mut state.drops.suppressed);
        state.drops.last = Some(now);
        let first = events.first().copied().unwrap_or_else(InputEvent::syn);
        tracing::warn!(
            target: "ternvale::virtio::input",
            device = self.kind.short(),
            reason,
            events = count,
            ev_type = codes::type_name(first.ev_type),
            code = %format!("{:#x}", first.code),
            pending = state.pending.len(),
            suppressed,
            "virtio-input event dropped"
        );
    }
}

/// Cloneable handle that injects events into one virtio-input device.
///
/// It does not keep the device (or guest memory) alive: once the VM drops the
/// device, reports are dropped.
#[derive(Clone)]
pub struct InputSink {
    pub(super) shared: Weak<Shared>,
    pub(super) kind: InputKind,
    pub(super) stats: Arc<InputStats>,
}

impl InputSink {
    pub(super) fn new(shared: &Arc<Shared>) -> Self {
        Self {
            shared: Arc::downgrade(shared),
            kind: shared.kind,
            stats: Arc::clone(&shared.stats),
        }
    }

    /// Keyboard or tablet.
    pub fn kind(&self) -> InputKind {
        self.kind
    }

    /// Live counters.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::input", skip_all)]
    pub fn stats(&self) -> Arc<InputStats> {
        Arc::clone(&self.stats)
    }

    /// Queue `events` followed by `SYN_REPORT`. Returns false when the report
    /// was dropped.
    #[tracing::instrument(
        level = "trace",
        target = "ternvale::virtio::input",
        skip_all,
        fields(device = self.kind.short(), events = events.len())
    )]
    pub fn report(&self, events: &[InputEvent]) -> bool {
        let mut report = events.to_vec();
        report.push(InputEvent::syn());
        let Some(shared) = self.shared.upgrade() else {
            self.stats
                .dropped
                .fetch_add(report.len() as u64, Ordering::Relaxed);
            tracing::debug!(
                target: "ternvale::virtio::input",
                device = self.kind.short(),
                events = report.len(),
                "virtio-input device is gone (VM stopped); report dropped"
            );
            return false;
        };
        let shared = &shared;
        shared.serve_kicks();
        let mut state = shared.lock_state();
        if state.pending.len() + report.len() > MAX_PENDING {
            // Buffers the guest re-posted since the last kick may be waiting.
            shared.flush(&mut state);
        }
        let reason = if let Some(bad) = events
            .iter()
            .find(|e| !shared.kind.supports(e.ev_type, e.code))
        {
            tracing::debug!(
                target: "ternvale::virtio::input",
                device = shared.kind.short(),
                ev_type = codes::type_name(bad.ev_type),
                code = %format!("{:#x}", bad.code),
                "event not advertised by this device"
            );
            Some("unsupported event")
        } else if state.eventq.is_none() {
            Some("driver not ready")
        } else if state.pending.len() + report.len() > MAX_PENDING {
            if state.releases_held_keys(&report) {
                tracing::debug!(
                    target: "ternvale::virtio::input",
                    device = shared.kind.short(),
                    pending = state.pending.len(),
                    "virtio-input backlog full; queuing a held key's release anyway"
                );
                None
            } else {
                Some("queue full")
            }
        } else {
            None
        };
        let queued = if let Some(reason) = reason {
            shared.drop_report(&mut state, &report, reason);
            false
        } else {
            state.track_keys(&report);
            state.pending.extend(report);
            shared.flush(&mut state);
            true
        };
        drop(state);
        shared.serve_kicks();
        queued
    }

    /// Key or button press/release.
    pub fn key(&self, code: u16, pressed: bool) -> bool {
        self.report(&[InputEvent::key(code, pressed)])
    }

    /// Absolute pointer position, each axis clamped to `0..=32767`.
    pub fn move_to(&self, x: u32, y: u32) -> bool {
        self.report(&[
            InputEvent::abs(ABS_X, x.min(ABS_MAX_VALUE)),
            InputEvent::abs(ABS_Y, y.min(ABS_MAX_VALUE)),
        ])
    }

    /// Wheel clicks: `dy > 0` scrolls up, `dx > 0` scrolls right.
    pub fn scroll(&self, dx: i32, dy: i32) -> bool {
        let mut events = Vec::with_capacity(2);
        if dy != 0 {
            events.push(InputEvent::rel(REL_WHEEL, dy));
        }
        if dx != 0 {
            events.push(InputEvent::rel(REL_HWHEEL, dx));
        }
        events.is_empty() || self.report(&events)
    }

    /// Events waiting for guest buffers.
    pub fn pending(&self) -> usize {
        self.shared
            .upgrade()
            .map_or(0, |shared| shared.lock_state().pending.len())
    }
}
