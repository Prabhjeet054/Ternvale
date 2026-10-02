//! Protocol messages.

use serde::{Deserialize, Serialize};

use crate::{MAX_DIMENSION, MAX_NAME};

/// One protocol message. JSON form: `{"type":"<snake_case name>", ...fields}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// Agent → host, first message on every connection.
    Hello {
        /// Highest protocol version the agent speaks.
        version: u32,
        /// Guest OS description, e.g. `Linux 6.6.58-0-virt aarch64`.
        os: String,
        /// Agent build, e.g. `ternvale-agent 0.1.0`. Optional on the wire.
        #[serde(default)]
        agent: String,
    },
    /// Host → agent: handshake accepted; both sides now speak `version`.
    Welcome {
        /// Negotiated protocol version.
        version: u32,
    },
    /// Host → agent: handshake refused; the host closes the connection.
    Reject {
        /// Why, for the agent's log.
        reason: String,
        /// Oldest version the host accepts.
        min_version: u32,
        /// Newest version the host accepts.
        max_version: u32,
    },
    /// Liveness probe; the receiver answers `Pong` with the same `seq`.
    Ping {
        /// Sender-chosen sequence number.
        seq: u64,
    },
    /// Answer to `Ping`.
    Pong {
        /// The `seq` of the `Ping` being answered.
        seq: u64,
    },
    /// Host → agent: resize the guest display.
    SetResolution {
        /// Pixels.
        width: u32,
        /// Pixels.
        height: u32,
    },
    /// Host → agent: replace the guest clipboard.
    ClipboardSet {
        /// New clipboard contents.
        text: ClipboardText,
    },
    /// Host → agent: flush and power the guest off.
    Shutdown,
}

/// Clipboard contents. `Debug` prints only the length, so logging a message
/// never copies the user's clipboard into the logs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClipboardText(pub String);

impl std::fmt::Debug for ClipboardText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "<{} bytes>", self.0.len())
    }
}

impl Message {
    /// The wire `type` tag.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Welcome { .. } => "welcome",
            Self::Reject { .. } => "reject",
            Self::Ping { .. } => "ping",
            Self::Pong { .. } => "pong",
            Self::SetResolution { .. } => "set_resolution",
            Self::ClipboardSet { .. } => "clipboard_set",
            Self::Shutdown => "shutdown",
        }
    }

    /// Range checks on peer-supplied fields. `Err` names the problem.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Hello { os, agent, .. } if os.len() > MAX_NAME || agent.len() > MAX_NAME => {
                Err(format!("hello os/agent longer than {MAX_NAME} bytes"))
            }
            Self::Reject { reason, .. } if reason.len() > MAX_NAME => {
                Err(format!("reject reason longer than {MAX_NAME} bytes"))
            }
            Self::SetResolution { width, height }
                if !(1..=MAX_DIMENSION).contains(width)
                    || !(1..=MAX_DIMENSION).contains(height) =>
            {
                Err(format!(
                    "resolution {width}x{height} outside 1..={MAX_DIMENSION}"
                ))
            }
            _ => Ok(()),
        }
    }
}
