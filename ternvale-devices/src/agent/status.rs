//! Agent connection state and the bookkeeping behind [`AgentStatus`].

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// Where the agent connection is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// No agent has connected yet.
    Waiting,
    /// Connected; waiting for `Hello`.
    Handshake,
    /// Handshake done; heartbeats running.
    Connected,
    /// An agent was connected and is gone (it may reconnect).
    Disconnected,
}

impl AgentState {
    /// Lowercase name used in logs, `ternvale status`, and JSON.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Handshake => "handshake",
            Self::Connected => "connected",
            Self::Disconnected => "disconnected",
        }
    }
}

/// Point-in-time view of the agent connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatus {
    /// Connection state.
    pub state: AgentState,
    /// Negotiated protocol version (while connected).
    pub version: Option<u32>,
    /// Guest OS from `Hello` (while connected).
    pub os: Option<String>,
    /// Agent build from `Hello` (while connected).
    pub agent: Option<String>,
    /// Time since the handshake.
    pub connected_ms: Option<u64>,
    /// Time since the last `Pong`.
    pub last_pong_ms: Option<u64>,
    /// Round trip of the last answered `Ping`, in microseconds.
    pub rtt_us: Option<u64>,
    /// Completed handshakes.
    pub connects: u64,
    /// Connections lost after a handshake.
    pub disconnects: u64,
    /// Heartbeat pings sent.
    pub pings: u64,
    /// Matching pongs received.
    pub pongs: u64,
    /// Why the last connection ended or failed.
    pub last_error: Option<String>,
}

/// The current connection.
pub(super) struct Conn {
    pub id: u64,
    pub writer: UnixStream,
    pub hello: Option<(u32, String, String)>,
    pub since: Instant,
    pub next_seq: u64,
    pub outstanding: Option<(u64, Instant)>,
    pub last_ping: Instant,
    pub last_pong: Option<Instant>,
    pub rtt: Option<Duration>,
    pub failure: Option<String>,
}

impl Conn {
    pub fn new(id: u64, writer: UnixStream) -> Self {
        let now = Instant::now();
        Self {
            id,
            writer,
            hello: None,
            since: now,
            next_seq: 1,
            outstanding: None,
            last_ping: now,
            last_pong: None,
            rtt: None,
            failure: None,
        }
    }

    /// Shut the socket so the session thread's read ends; `why` becomes the
    /// recorded error unless one is already set.
    pub fn fail(&mut self, why: String) {
        self.failure.get_or_insert(why);
        if let Err(error) = self.writer.shutdown(std::net::Shutdown::Both) {
            tracing::debug!(target: "ternvale::agent", conn = self.id, error = %error, "agent socket shutdown failed");
        }
    }
}

/// Everything behind [`AgentStatus`], guarded by one mutex.
#[derive(Default)]
pub(super) struct Book {
    pub conn: Option<Conn>,
    pub ever_connected: bool,
    pub connects: u64,
    pub disconnects: u64,
    pub pings: u64,
    pub pongs: u64,
    pub last_error: Option<String>,
}

impl Book {
    /// The connection `id`, if it is still the current one.
    pub fn conn(&mut self, id: u64) -> Option<&mut Conn> {
        self.conn.as_mut().filter(|conn| conn.id == id)
    }

    pub fn snapshot(&self, now: Instant) -> AgentStatus {
        let ms = |since: Instant| millis(now.saturating_duration_since(since));
        let conn = self.conn.as_ref();
        let hello = conn.and_then(|conn| conn.hello.as_ref());
        let state = match (conn, hello) {
            (Some(_), Some(_)) => AgentState::Connected,
            (Some(_), None) => AgentState::Handshake,
            (None, _) if self.ever_connected => AgentState::Disconnected,
            (None, _) => AgentState::Waiting,
        };
        AgentStatus {
            state,
            version: hello.map(|hello| hello.0),
            os: hello.map(|hello| hello.1.clone()),
            agent: hello.map(|hello| hello.2.clone()),
            connected_ms: conn.filter(|_| hello.is_some()).map(|conn| ms(conn.since)),
            last_pong_ms: conn.and_then(|conn| conn.last_pong).map(ms),
            rtt_us: conn
                .and_then(|conn| conn.rtt)
                .map(|rtt| u64::try_from(rtt.as_micros()).unwrap_or(u64::MAX)),
            connects: self.connects,
            disconnects: self.disconnects,
            pings: self.pings,
            pongs: self.pongs,
            last_error: self.last_error.clone(),
        }
    }
}

pub(super) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
