//! One connection: handshake, then answer host messages until it ends.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ternvale_agent_proto::{
    negotiate, write_message, FrameError, FrameReader, Message, PROTOCOL_VERSION,
};

use crate::{os_description, Actions, AgentConfig, AgentError, Stream};

/// How a session ended.
#[derive(Debug)]
pub enum SessionEnd {
    /// The host asked for shutdown and the action returned.
    Shutdown,
    /// `stop` was set.
    Stopped,
    /// The connection failed or closed; `handshaken` says whether it got as
    /// far as `Welcome` (which resets the backoff).
    Ended {
        /// The host welcomed us on this connection.
        handshaken: bool,
        /// Why it ended.
        error: AgentError,
    },
}

/// Serve one connection.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(addr = %config.target))]
pub fn serve(
    mut stream: Stream,
    config: &AgentConfig,
    actions: &mut dyn Actions,
    stop: &AtomicBool,
) -> SessionEnd {
    let mut reader = FrameReader::new("host");
    let end = match handshake(&mut stream, &mut reader, config, stop) {
        Ok(None) => SessionEnd::Stopped,
        Err(error) => SessionEnd::Ended {
            handshaken: false,
            error,
        },
        Ok(Some(version)) => {
            tracing::info!(target: "ternvale::agent", version, addr = %config.target, "agent connected to host");
            match serve_requests(&mut stream, &mut reader, config, actions, stop) {
                Ok(end) => end,
                Err(error) => SessionEnd::Ended {
                    handshaken: true,
                    error,
                },
            }
        }
    };
    stream.close();
    if let SessionEnd::Ended { error, handshaken } = &end {
        tracing::info!(target: "ternvale::agent", handshaken, error = %error, "agent session ended");
    }
    end
}

/// Send `Hello` and wait for `Welcome`. `Ok(None)` if `stop` was set.
fn handshake(
    stream: &mut Stream,
    reader: &mut FrameReader,
    config: &AgentConfig,
    stop: &AtomicBool,
) -> Result<Option<u32>, AgentError> {
    stream
        .set_read_timeout(Some(config.poll))
        .map_err(|source| AgentError::Frame {
            op: "set read timeout",
            source: source.into(),
        })?;
    let hello = Message::Hello {
        version: PROTOCOL_VERSION,
        os: os_description(),
        agent: format!("ternvale-agent {}", env!("CARGO_PKG_VERSION")),
    };
    write_message(stream, &hello, "host").map_err(|source| AgentError::Frame {
        op: "send hello",
        source,
    })?;
    let deadline = Instant::now() + config.handshake_timeout;
    loop {
        match next(stream, reader, stop, deadline, "read welcome")? {
            Next::Stopped => return Ok(None),
            Next::TimedOut => {
                return Err(AgentError::HandshakeTimeout {
                    ms: millis(config.handshake_timeout),
                })
            }
            Next::Message(Message::Welcome { version }) => return Ok(Some(negotiate(version)?)),
            Next::Message(Message::Reject {
                reason,
                min_version,
                max_version,
            }) => {
                tracing::error!(target: "ternvale::agent", reason = %reason, min_version, max_version, ours = PROTOCOL_VERSION, "host rejected the agent");
                return Err(AgentError::Rejected {
                    reason,
                    min_version,
                    max_version,
                });
            }
            Next::Message(other) => {
                tracing::warn!(target: "ternvale::agent", kind = other.kind(), "message before welcome ignored");
            }
        }
    }
}

fn serve_requests(
    stream: &mut Stream,
    reader: &mut FrameReader,
    config: &AgentConfig,
    actions: &mut dyn Actions,
    stop: &AtomicBool,
) -> Result<SessionEnd, AgentError> {
    loop {
        let deadline = Instant::now() + config.idle_timeout;
        let message = match next(stream, reader, stop, deadline, "read request")? {
            Next::Stopped => return Ok(SessionEnd::Stopped),
            Next::TimedOut => {
                tracing::warn!(target: "ternvale::agent", idle_ms = millis(config.idle_timeout), "host silent; reconnecting");
                return Err(AgentError::Idle {
                    ms: millis(config.idle_timeout),
                });
            }
            Next::Message(message) => message,
        };
        let result = match message {
            Message::Ping { seq } => {
                write_message(stream, &Message::Pong { seq }, "host").map_err(|source| {
                    AgentError::Frame {
                        op: "send pong",
                        source,
                    }
                })
            }
            Message::SetResolution { width, height } => actions.set_resolution(width, height),
            Message::ClipboardSet { text } => actions.clipboard_set(&text.0),
            Message::Shutdown => {
                actions.shutdown()?;
                return Ok(SessionEnd::Shutdown);
            }
            other @ (Message::Hello { .. }
            | Message::Welcome { .. }
            | Message::Reject { .. }
            | Message::Pong { .. }) => {
                tracing::warn!(target: "ternvale::agent", kind = other.kind(), "unexpected message from host ignored");
                Ok(())
            }
        };
        match result {
            Err(error @ AgentError::Frame { .. }) => return Err(error),
            Err(error) => {
                tracing::warn!(target: "ternvale::agent", error = %error, "host request failed");
            }
            Ok(()) => {}
        }
    }
}

enum Next {
    Message(Message),
    Stopped,
    TimedOut,
}

/// The next message, skipping unknown/malformed frames, until `deadline`.
fn next(
    stream: &mut Stream,
    reader: &mut FrameReader,
    stop: &AtomicBool,
    deadline: Instant,
    op: &'static str,
) -> Result<Next, AgentError> {
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(Next::Stopped);
        }
        match reader.read(stream) {
            Ok(message) => return Ok(Next::Message(message)),
            Err(error) if error.is_skippable() => {}
            Err(error) if error.is_timeout() => {
                if Instant::now() >= deadline {
                    return Ok(Next::TimedOut);
                }
            }
            Err(FrameError::Closed) => return Err(AgentError::Disconnected),
            Err(source) => return Err(AgentError::Frame { op, source }),
        }
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
