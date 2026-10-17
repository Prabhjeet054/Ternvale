//! Rolling presentation statistics: every [`PresentConfig::report_every`] a
//! viewer session logs DEBUG `display present window` with the frames shown,
//! frames dropped, and the mean and worst flush-to-present latency of that
//! window, then starts a new one.
//!
//! [`PresentConfig::report_every`]: crate::PresentConfig::report_every

use std::time::{Duration, Instant};

/// One reporting window's counters.
#[derive(Debug)]
pub(crate) struct LatencyWindow {
    started: Instant,
    presented: u64,
    dropped: u64,
    sum_us: u64,
    samples: u64,
    max_us: u64,
}

impl LatencyWindow {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            started: now,
            presented: 0,
            dropped: 0,
            sum_us: 0,
            samples: 0,
            max_us: 0,
        }
    }

    /// A frame reached the screen; `flush_to_present_us` if it had a flush stamp.
    pub(crate) fn presented(&mut self, flush_to_present_us: Option<u64>) {
        self.presented += 1;
        if let Some(us) = flush_to_present_us {
            self.sum_us = self.sum_us.saturating_add(us);
            self.samples += 1;
            self.max_us = self.max_us.max(us);
        }
    }

    /// Guest updates that were never sent.
    pub(crate) fn dropped(&mut self, count: u64) {
        self.dropped += count;
    }

    /// Mean flush-to-present latency in this window, µs.
    pub(crate) fn average_us(&self) -> Option<u64> {
        (self.samples > 0).then(|| self.sum_us / self.samples)
    }

    /// Log and restart the window if `every` has passed since it began.
    /// Returns whether it logged.
    pub(crate) fn report_if_due(&mut self, now: Instant, every: Duration) -> bool {
        if now.saturating_duration_since(self.started) < every {
            return false;
        }
        self.report(now, "interval");
        true
    }

    /// Log whatever the window holds (a session ending mid-window).
    pub(crate) fn report(&mut self, now: Instant, why: &'static str) {
        let window = now.saturating_duration_since(self.started);
        let window_ms = u64::try_from(window.as_millis()).unwrap_or(u64::MAX);
        let fps = if window.is_zero() {
            0.0
        } else {
            self.presented as f64 / window.as_secs_f64()
        };
        tracing::debug!(target: "ternvale::gpu", why, window_ms, presented = self.presented,
            fps = %format!("{fps:.1}"), dropped = self.dropped,
            avg_flush_to_present_us = self.average_us(), max_flush_to_present_us = self.max_us,
            "display present window");
        *self = Self::new(now);
    }
}
