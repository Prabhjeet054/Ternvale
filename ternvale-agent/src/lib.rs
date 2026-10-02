//! `ternvale-agent`: runs inside the guest, connects to the host agent server
//! (vsock CID 2, port 5000), says `Hello`, answers `Ping`, and carries out
//! host requests. Reconnects with exponential backoff whenever the
//! connection fails or drops.
//!
//! The crate also builds on macOS with the Unix-socket transport so host
//! tests can run the real agent loop against the host server without a VM.

mod actions;
mod backoff;
mod error;
mod os;
mod session;
mod transport;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use actions::{Actions, SystemActions};
pub use backoff::Backoff;
pub use error::AgentError;
pub use os::os_description;
pub use session::SessionEnd;
pub use transport::{Stream, Target};

/// How the agent connects and when it gives up on a connection.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Where the host listens.
    pub target: Target,
    /// Reconnect delays.
    pub backoff: Backoff,
    /// How long to wait for `Welcome` after `Hello`.
    pub handshake_timeout: Duration,
    /// Reconnect when nothing arrives for this long (the host pings every 2 s).
    pub idle_timeout: Duration,
    /// Socket read timeout; bounds how quickly `stop` is noticed.
    pub poll: Duration,
}

impl AgentConfig {
    /// Defaults for `target`: 200 ms → 5 s backoff, 5 s handshake, 10 s idle.
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(addr = %target))]
    pub fn new(target: Target) -> Self {
        Self {
            target,
            backoff: Backoff::new(Duration::from_millis(200), Duration::from_secs(5)),
            handshake_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(10),
            poll: Duration::from_millis(250),
        }
    }
}

/// Why [`run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The host sent `Shutdown` and the action ran (off Linux, or in tests).
    Shutdown,
    /// `stop` was set.
    Stopped,
}

/// Connect, serve, reconnect with backoff; until `Shutdown` or `stop`.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(addr = %config.target))]
pub fn run(config: &AgentConfig, actions: &mut dyn Actions, stop: &AtomicBool) -> Exit {
    let mut backoff = config.backoff.clone();
    let mut failures: u64 = 0;
    tracing::info!(target: "ternvale::agent", addr = %config.target, version = ternvale_agent_proto::PROTOCOL_VERSION, "agent starting");
    loop {
        if stop.load(Ordering::Acquire) {
            tracing::info!(target: "ternvale::agent", "agent stopped");
            return Exit::Stopped;
        }
        let error = match transport::connect(&config.target) {
            Err(error) => error,
            Ok(stream) => match session::serve(stream, config, actions, stop) {
                SessionEnd::Shutdown => return Exit::Shutdown,
                SessionEnd::Stopped => continue,
                SessionEnd::Ended { handshaken, error } => {
                    if handshaken {
                        backoff.reset();
                        failures = 0;
                    }
                    error
                }
            },
        };
        failures += 1;
        let delay = backoff.next_delay();
        tracing::info!(target: "ternvale::agent", failures, delay_ms = delay.as_millis() as u64, error = %error, "agent not connected; retrying");
        sleep_unless_stopped(delay, stop);
    }
}

fn sleep_unless_stopped(total: Duration, stop: &AtomicBool) {
    let step = Duration::from_millis(20);
    let mut slept = Duration::ZERO;
    while slept < total && !stop.load(Ordering::Acquire) {
        let nap = step.min(total - slept);
        std::thread::sleep(nap);
        slept += nap;
    }
}

#[cfg(test)]
mod tests;
