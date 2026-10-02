//! Exponential reconnect backoff.

use std::time::Duration;

/// Doubling delay from `initial` up to `max`; [`Backoff::reset`] after a
/// successful handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    initial: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    /// First delay `initial`, never more than `max`.
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(initial_ms = initial.as_millis() as u64, max_ms = max.as_millis() as u64))]
    pub fn new(initial: Duration, max: Duration) -> Self {
        let initial = initial.min(max);
        Self {
            initial,
            max,
            next: initial,
        }
    }

    /// The delay to wait now; doubles the following one.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = self.next.saturating_mul(2).min(self.max);
        delay
    }

    /// Start again from `initial`.
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
    pub fn reset(&mut self) {
        if self.next != self.initial {
            tracing::debug!(target: "ternvale::agent", initial_ms = self.initial.as_millis() as u64, "backoff reset");
        }
        self.next = self.initial;
    }
}
