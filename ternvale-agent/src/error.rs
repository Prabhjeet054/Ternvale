//! Agent errors.

use ternvale_agent_proto::{FrameError, VersionError};

/// Why a connection attempt or session ended.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Could not connect to the host.
    #[error("connect to {target}: {source}")]
    Connect {
        /// Where.
        target: String,
        /// Socket error (ECONNRESET when no host server is listening).
        #[source]
        source: std::io::Error,
    },
    /// The requested transport does not exist on this OS.
    #[error("{0} transport is not available on this OS")]
    Unsupported(&'static str),
    /// Reading or writing a frame failed.
    #[error("{op}: {source}")]
    Frame {
        /// What the agent was doing.
        op: &'static str,
        /// Framing error.
        #[source]
        source: FrameError,
    },
    /// The host refused our version.
    #[error("host rejected the handshake: {reason} (host speaks {min_version}..={max_version})")]
    Rejected {
        /// Host's reason.
        reason: String,
        /// Host's oldest version.
        min_version: u32,
        /// Host's newest version.
        max_version: u32,
    },
    /// The host welcomed us with a version we cannot speak.
    #[error("host chose an unusable version: {0}")]
    Version(#[from] VersionError),
    /// No `Welcome` in time.
    #[error("no welcome from the host within {ms} ms")]
    HandshakeTimeout {
        /// Timeout.
        ms: u64,
    },
    /// Nothing from the host for too long.
    #[error("no message from the host for {ms} ms")]
    Idle {
        /// Timeout.
        ms: u64,
    },
    /// The host closed the connection.
    #[error("host closed the connection")]
    Disconnected,
    /// A host request could not be carried out.
    #[error("{action}: {source}")]
    Action {
        /// Which request.
        action: &'static str,
        /// OS error.
        #[source]
        source: std::io::Error,
    },
}
