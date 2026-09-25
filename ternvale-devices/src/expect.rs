//! Expect-style steps over a growing guest serial transcript.
//!
//! An [`Expect`] matches only text that arrived after the previous step, so a
//! banner that already contains a later pattern does not satisfy that step.

use std::time::{Duration, Instant};

/// One harness action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Wait until `pattern` appears in serial text produced after this step starts.
    Expect {
        /// Substring required in the new serial text.
        pattern: String,
        /// How long to wait before this step fails.
        timeout: Duration,
    },
    /// Bytes to write to the guest once the previous step has settled.
    Send {
        /// Host-to-guest bytes. Callers keep each write within the PL011 FIFO.
        data: Vec<u8>,
    },
}

/// What the runner should do on this poll.
#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    /// The current expect has not matched yet.
    Pending,
    /// Write these bytes, then poll again.
    Send(Vec<u8>),
    /// Every step completed.
    Done,
    /// The current expect exceeded its timeout.
    TimedOut {
        /// Index of the step that timed out.
        step: usize,
        /// Last lines of the serial transcript.
        last_lines: String,
    },
}

/// Cursor over [`Step`] values.
#[derive(Debug)]
pub struct Session {
    steps: Vec<Step>,
    index: usize,
    step_at: Instant,
    mark: Option<usize>,
}

impl Session {
    /// Start at the first step. `now` is the clock for the first timeout.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(steps = steps.len()))]
    pub fn new(steps: Vec<Step>, now: Instant) -> Self {
        tracing::debug!(target: "ternvale::boot", steps = steps.len(), "expect session");
        Self {
            steps,
            index: 0,
            step_at: now,
            mark: None,
        }
    }

    /// Advance using `serial` as the transcript so far.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(step = self.index))]
    pub fn poll(&mut self, serial: &str, now: Instant) -> Progress {
        if self.index >= self.steps.len() {
            return Progress::Done;
        }
        match &self.steps[self.index] {
            Step::Send { data } => {
                let data = data.clone();
                let end = serial.len();
                self.advance(now);
                // The following expect ignores text that was already on the console.
                self.mark = Some(end);
                tracing::debug!(
                    target: "ternvale::boot",
                    bytes = data.len(),
                    "expect send"
                );
                Progress::Send(data)
            }
            Step::Expect { pattern, timeout } => {
                let mark = self.mark.unwrap_or(0);
                let fresh = serial.get(mark..).unwrap_or("");
                if fresh.contains(pattern) {
                    tracing::info!(
                        target: "ternvale::boot",
                        step = self.index,
                        pattern,
                        "expect matched"
                    );
                    self.advance(now);
                    self.poll(serial, now)
                } else if now.saturating_duration_since(self.step_at) >= *timeout {
                    tracing::error!(
                        target: "ternvale::boot",
                        step = self.index,
                        pattern,
                        "expect timed out"
                    );
                    Progress::TimedOut {
                        step: self.index,
                        last_lines: last_lines(serial, 20),
                    }
                } else {
                    Progress::Pending
                }
            }
        }
    }
}

impl Session {
    fn advance(&mut self, now: Instant) {
        self.index += 1;
        self.step_at = now;
        self.mark = None;
    }

    /// True when the next action is a [`Step::Send`].
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
    pub fn waiting_to_send(&self) -> bool {
        matches!(self.steps.get(self.index), Some(Step::Send { .. }))
    }
}

/// Last `n` lines, keeping a short tail when the transcript is long.
pub fn last_lines(serial: &str, n: usize) -> String {
    let lines: Vec<&str> = serial.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::{last_lines, Progress, Session, Step};
    use std::time::{Duration, Instant};

    fn expect(pattern: &str, ms: u64) -> Step {
        Step::Expect {
            pattern: pattern.to_string(),
            timeout: Duration::from_millis(ms),
        }
    }

    #[test]
    fn banner_match_does_not_satisfy_a_later_expect() {
        let start = Instant::now();
        let mut session = Session::new(
            vec![
                expect("Linux version", 1000),
                Step::Send {
                    data: b"uname -a\n".to_vec(),
                },
                expect("aarch64", 1000),
            ],
            start,
        );
        let early = "Linux version 6.6 (build-aarch64)\n";
        assert!(matches!(
            session.poll(early, start),
            Progress::Send(data) if data == b"uname -a\n".to_vec()
        ));
        assert!(matches!(session.poll(early, start), Progress::Pending));
        let later = format!("{early}Linux build-aarch64 6.6 aarch64 GNU/Linux\n");
        assert_eq!(session.poll(&later, start), Progress::Done);
    }

    #[test]
    fn timeout_reports_the_step_and_the_last_lines() {
        let start = Instant::now();
        let mut session = Session::new(vec![expect("Linux version", 10)], start);
        let serial = "random\nearlycon\n";
        let later = start + Duration::from_millis(11);
        match session.poll(serial, later) {
            Progress::TimedOut { step, last_lines } => {
                assert_eq!(step, 0);
                assert!(last_lines.contains("earlycon"));
            }
            other => panic!("expected timeout, got {other:?}"),
        }
    }

    #[test]
    fn last_lines_keeps_the_tail() {
        let serial = (0..30)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let tail = last_lines(&serial, 20);
        assert!(tail.starts_with("10\n"));
        assert!(tail.ends_with("29"));
    }
}
