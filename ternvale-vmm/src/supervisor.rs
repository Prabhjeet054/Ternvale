//! VMM-process supervisor: exponential backoff and a 3-crashes-in-5-minutes stop.

use std::time::Duration;

/// Stop restarting after this many unexpected exits in [`CRASH_WINDOW`].
pub const MAX_CRASHES: usize = 3;
/// Sliding window for [`MAX_CRASHES`].
pub const CRASH_WINDOW: Duration = Duration::from_secs(5 * 60);
/// First restart delay.
pub const BACKOFF_BASE: Duration = Duration::from_secs(1);

/// Decision after an unexpected VMM exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuperviseDecision {
    /// Restart after `backoff`.
    Restart { backoff: Duration, crash_n: u32 },
    /// Give up and notify the user.
    Stop { reason: &'static str, crashes: u32 },
}

/// Per-VM crash history (monotonic `now` from the caller).
#[derive(Debug, Default, Clone)]
pub struct VmmSupervisor {
    crashes: Vec<Duration>,
    consecutive: u32,
}

impl VmmSupervisor {
    /// Empty history.
    #[must_use]
    #[tracing::instrument(level = "debug", target = "ternvale::watchdog", skip_all)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an unexpected VMM process death at `now` (monotonic).
    #[tracing::instrument(level = "debug", target = "ternvale::watchdog", skip_all, fields(now_ms = now.as_millis() as u64))]
    pub fn note_crash(&mut self, now: Duration) -> SuperviseDecision {
        self.crashes.push(now);
        self.crashes
            .retain(|t| now.saturating_sub(*t) <= CRASH_WINDOW);
        self.consecutive = self.consecutive.saturating_add(1);
        let n = self.crashes.len() as u32;
        if self.crashes.len() >= MAX_CRASHES {
            tracing::error!(
                target: "ternvale::watchdog",
                crashes = n,
                window_secs = CRASH_WINDOW.as_secs(),
                reason = "vmm process died 3 times in 5 minutes",
                "supervisor stopping restarts"
            );
            return SuperviseDecision::Stop {
                reason: "VMM process died 3 times in 5 minutes",
                crashes: n,
            };
        }
        let shift = self.consecutive.saturating_sub(1).min(6);
        let backoff = BACKOFF_BASE.saturating_mul(1u32 << shift);
        tracing::error!(
            target: "ternvale::watchdog",
            crashes = n,
            consecutive = self.consecutive,
            backoff_ms = backoff.as_millis() as u64,
            reason = "vmm process died unexpectedly",
            "supervisor will restart the VM"
        );
        SuperviseDecision::Restart {
            backoff,
            crash_n: n,
        }
    }

    /// Guest ran long enough that the next crash is not consecutive.
    #[tracing::instrument(level = "debug", target = "ternvale::watchdog", skip_all)]
    pub fn note_healthy(&mut self) {
        self.consecutive = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_in_five_minutes_stops_and_backoff_doubles() {
        let mut s = VmmSupervisor::new();
        let a = s.note_crash(Duration::from_secs(0));
        match a {
            SuperviseDecision::Restart { backoff, crash_n } => {
                assert_eq!(backoff, Duration::from_secs(1));
                assert_eq!(crash_n, 1);
            }
            other => panic!("{other:?}"),
        }
        let b = s.note_crash(Duration::from_secs(10));
        match b {
            SuperviseDecision::Restart { backoff, .. } => {
                assert_eq!(backoff, Duration::from_secs(2));
            }
            other => panic!("{other:?}"),
        }
        let c = s.note_crash(Duration::from_secs(20));
        match c {
            SuperviseDecision::Stop { crashes, reason } => {
                assert_eq!(crashes, 3);
                assert!(reason.contains("3 times"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn crashes_outside_the_window_do_not_stop() {
        let mut s = VmmSupervisor::new();
        assert!(matches!(
            s.note_crash(Duration::from_secs(0)),
            SuperviseDecision::Restart { .. }
        ));
        assert!(matches!(
            s.note_crash(Duration::from_secs(10)),
            SuperviseDecision::Restart { .. }
        ));
        let later = s.note_crash(Duration::from_secs(5 * 60 + 11));
        assert!(
            matches!(later, SuperviseDecision::Restart { crash_n: 1, .. }),
            "{later:?}"
        );
    }
}
