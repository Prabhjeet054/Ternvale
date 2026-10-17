//! virtio-input backlog limits and eventq kicks under contention (Step 65).

use super::tests::{Fx, WRITE};
use super::*;

#[test]
fn full_backlog_still_takes_releases_of_held_keys() {
    let mut fx = Fx::new(InputKind::Keyboard);
    fx.kick(0);
    assert!(fx.sink.key(42, true), "shift down");
    for i in 0..MAX_PENDING / 2 - 1 {
        assert!(fx.sink.key(30, i % 2 == 0), "report {i} queued");
    }
    assert_eq!(fx.sink.pending(), MAX_PENDING);
    assert!(!fx.sink.key(31, true), "a press is dropped when full");
    assert!(!fx.sink.key(31, false), "its release was never held");
    assert!(fx.sink.key(42, false), "the held key's release is queued");
    assert_eq!(fx.sink.pending(), MAX_PENDING + 2);
    assert!(!fx.sink.key(42, false), "a second release is not held");
    assert_eq!(fx.sink.stats().snapshot(), [0, 6, 0]);

    fx.post(0, 0, 16, WRITE);
    fx.kick(0);
    assert_eq!(fx.sink.pending(), MAX_PENDING + 2 - 16);
}

#[test]
fn full_backlog_drains_into_buffers_posted_without_a_kick() {
    let mut fx = Fx::new(InputKind::Keyboard);
    fx.kick(0);
    for i in 0..MAX_PENDING / 2 {
        assert!(fx.sink.key(30, i % 2 == 0), "report {i} queued");
    }
    fx.post(0, 0, 16, WRITE);
    assert!(fx.sink.key(31, true), "posted buffers make room");
    assert_eq!(fx.used_idx(0), 16);
    assert_eq!(fx.sink.pending(), MAX_PENDING - 16 + 2);
    assert_eq!(fx.sink.stats().snapshot(), [16, 0, 0]);
}

/// The vCPU's kick must not wait while the host thread holds the device
/// state (it may itself be waiting for guest memory). Kicking on the thread
/// that holds the lock would deadlock if it did.
#[test]
fn eventq_kick_never_waits_for_the_state_lock() {
    let mut fx = Fx::new(InputKind::Keyboard);
    fx.kick(0);
    assert!(fx.sink.key(30, true), "queued without buffers");
    let shared = fx.sink.shared.upgrade().expect("device alive");
    let held = shared.lock_state();
    fx.post(0, 0, 4, WRITE);
    fx.kick(0);
    assert_eq!(fx.used_idx(0), 0, "the kick is left to the lock holder");
    drop(held);
    shared.serve_kicks();
    assert_eq!(fx.used_idx(0), 2);
    assert_eq!(fx.sink.pending(), 0);
    assert!(fx.raised.load(std::sync::atomic::Ordering::Acquire));
}

/// A first kick (queue binding) deferred to the lock holder is served before
/// the next report checks whether the driver is ready.
#[test]
fn deferred_first_kick_binds_before_the_next_report() {
    let mut fx = Fx::new(InputKind::Keyboard);
    let shared = fx.sink.shared.upgrade().expect("device alive");
    let held = shared.lock_state();
    fx.post(0, 0, 4, WRITE);
    fx.kick(0);
    drop(held);
    assert!(fx.sink.key(30, true), "eventq is bound");
    assert_eq!(fx.used_idx(0), 2);
}
