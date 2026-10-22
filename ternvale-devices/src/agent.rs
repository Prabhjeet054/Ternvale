//! Host side of the guest agent protocol (`ternvale-agent-proto`).
//!
//! The in-guest `ternvale-agent` connects to vsock CID 2 port 5000; the
//! vsock device turns that into a connection to `<uds_dir>/host-5000.sock`,
//! which [`AgentServer`] listens on. One agent connection is current at a
//! time (a new one replaces the old). The server answers `Hello` with
//! `Welcome`/`Reject`, pings every [`AgentServerConfig::heartbeat`], and drops
//! the connection when a pong is overdue; the agent then reconnects.

mod service;
mod session;
mod status;

use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ternvale_agent_proto::{write_message, FrameError, Message};

use status::Book;
pub use status::{AgentState, AgentStatus};

/// Timing for [`AgentServer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentServerConfig {
    /// Interval between heartbeat pings.
    pub heartbeat: Duration,
    /// Drop the connection when a ping is unanswered this long.
    pub pong_timeout: Duration,
    /// Drop a new connection that sends no `Hello` within this.
    pub hello_timeout: Duration,
    /// Accept / read poll interval (bounds shutdown latency).
    pub poll: Duration,
}

impl Default for AgentServerConfig {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(2),
            pong_timeout: Duration::from_secs(6),
            hello_timeout: Duration::from_secs(5),
            poll: Duration::from_millis(50),
        }
    }
}

/// Agent server errors.
#[derive(Debug, thiserror::Error)]
pub enum AgentServerError {
    /// The listening socket could not be set up.
    #[error("agent socket {}: {source}", path.display())]
    Socket {
        /// Socket path.
        path: PathBuf,
        /// OS error.
        source: std::io::Error,
    },
    /// Another server answers on the socket.
    #[error("agent socket {} is in use by another process", path.display())]
    InUse {
        /// Socket path.
        path: PathBuf,
    },
    /// The service thread could not be started.
    #[error("spawn agent server thread: {source}")]
    Spawn {
        /// OS error.
        source: std::io::Error,
    },
    /// No agent is connected.
    #[error("agent is not connected (state {state})")]
    NotConnected {
        /// Current state name.
        state: &'static str,
    },
    /// The agent did not answer `ClipboardGet` in time.
    #[error("clipboard_get timed out after {timeout_ms} ms")]
    ClipboardTimeout {
        /// Wait duration in milliseconds.
        timeout_ms: u64,
    },
    /// Only host request types may be sent with [`AgentServer::send`].
    #[error("{kind} is not a host request")]
    NotARequest {
        /// Message type.
        kind: &'static str,
    },
    /// Writing to the agent failed.
    #[error("send {kind} to agent: {source}")]
    Send {
        /// Message type.
        kind: &'static str,
        /// Framing / socket error.
        source: FrameError,
    },
}

/// State shared by the service and session threads.
pub(crate) struct Shared {
    vm: String,
    config: AgentServerConfig,
    stop: AtomicBool,
    book: Mutex<Book>,
    /// virtio-9p shares to mount after handshake (tag, guest mount point).
    shares: Vec<(String, String)>,
}

impl Shared {
    fn book(&self) -> ternvale_vmm::lockwatch::Guard<'_, Book> {
        ternvale_vmm::lockwatch::lock(&self.book, "agent")
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Write `message` on connection `id` if it is still current.
    fn write(&self, id: u64, message: &Message) -> Result<(), AgentServerError> {
        let mut book = self.book();
        let Some(conn) = book.conn(id) else {
            return Err(AgentServerError::NotConnected { state: "replaced" });
        };
        write_message(&mut conn.writer, message, "agent").map_err(|source| AgentServerError::Send {
            kind: message.kind(),
            source,
        })
    }
}

/// Listens for the guest agent and tracks its connection. Dropping it stops
/// the threads, closes the connection, and removes the socket.
pub struct AgentServer {
    shared: Arc<Shared>,
    path: PathBuf,
    thread: Option<JoinHandle<()>>,
}

impl AgentServer {
    /// Listen on `path` (normally `host_socket_path(uds_dir, AGENT_PORT)`)
    /// with default timing.
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(vm, path = %path.display()))]
    pub fn start(path: &Path, vm: &str) -> Result<Self, AgentServerError> {
        Self::start_with(path, vm, AgentServerConfig::default(), Vec::new())
    }

    /// Listen on `path` with `config` timing.
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(vm, path = %path.display()))]
    pub fn start_with(
        path: &Path,
        vm: &str,
        config: AgentServerConfig,
        shares: Vec<(String, String)>,
    ) -> Result<Self, AgentServerError> {
        let fail = |source| socket_error(path, source);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| socket_error(dir, source))?;
        }
        clear_stale(path)?;
        let listener = UnixListener::bind(path).map_err(fail)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(fail)?;
        listener.set_nonblocking(true).map_err(fail)?;
        let shared = Arc::new(Shared {
            vm: vm.to_string(),
            config,
            stop: AtomicBool::new(false),
            book: Mutex::new(Book::default()),
            shares,
        });
        let service = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("agent-server".into())
            .spawn(move || service::run(&service, &listener))
            .map_err(|source| {
                tracing::error!(target: "ternvale::agent", error = %source, "agent server thread spawn failed");
                AgentServerError::Spawn { source }
            })?;
        tracing::info!(target: "ternvale::agent", vm, path = %path.display(), heartbeat_ms = status::millis(config.heartbeat), "agent server listening");
        Ok(Self {
            shared,
            path: path.to_path_buf(),
            thread: Some(thread),
        })
    }

    /// Socket path.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Current connection state and counters.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(vm = %self.shared.vm))]
    pub fn status(&self) -> AgentStatus {
        self.shared.book().snapshot(Instant::now())
    }

    /// Poll [`Self::status`] until `done` holds or `timeout` passes.
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(vm = %self.shared.vm, timeout_ms = status::millis(timeout)))]
    pub fn wait_for(
        &self,
        timeout: Duration,
        done: impl Fn(&AgentStatus) -> bool,
    ) -> Option<AgentStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.status();
            if done(&status) {
                return Some(status);
            }
            if Instant::now() >= deadline {
                tracing::debug!(target: "ternvale::agent", state = status.state.as_str(), "agent wait timed out");
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Send a host request (`SetResolution`, `ClipboardSet`, `Shutdown`) to
    /// the connected agent.
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(vm = %self.shared.vm, kind = message.kind()))]
    pub fn send(&self, message: &Message) -> Result<(), AgentServerError> {
        if !matches!(
            message,
            Message::SetResolution { .. }
                | Message::ClipboardSet { .. }
                | Message::ClipboardGet
                | Message::ShareMount { .. }
                | Message::Shutdown
        ) {
            return Err(AgentServerError::NotARequest {
                kind: message.kind(),
            });
        }
        let mut book = self.shared.book();
        let state = book.snapshot(Instant::now()).state;
        let conn = match book.conn.as_mut() {
            Some(conn) if state == AgentState::Connected => conn,
            _ => {
                tracing::warn!(target: "ternvale::agent", kind = message.kind(), state = state.as_str(), "agent request not sent; no agent connected");
                return Err(AgentServerError::NotConnected {
                    state: state.as_str(),
                });
            }
        };
        write_message(&mut conn.writer, message, "agent").map_err(|source| {
            tracing::warn!(target: "ternvale::agent", kind = message.kind(), error = %source, "agent request send failed");
            AgentServerError::Send {
                kind: message.kind(),
                source,
            }
        })?;
        tracing::info!(target: "ternvale::agent", kind = message.kind(), "agent request sent");
        Ok(())
    }

    /// Ask the guest for its clipboard text (protocol v2+).
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(vm = %self.shared.vm, timeout_ms = status::millis(timeout)))]
    pub fn clipboard_get(&self, timeout: Duration) -> Result<String, AgentServerError> {
        let (tx, rx) = mpsc::channel();
        {
            let mut book = self.shared.book();
            let state = book.snapshot(Instant::now()).state;
            if state != AgentState::Connected {
                return Err(AgentServerError::NotConnected {
                    state: state.as_str(),
                });
            }
            if book.clipboard_wait.is_some() {
                return Err(AgentServerError::NotConnected {
                    state: "clipboard_get already pending",
                });
            }
            book.clipboard_wait = Some(tx);
        }
        self.send(&Message::ClipboardGet)?;
        match rx.recv_timeout(timeout) {
            Ok(Ok(text)) => {
                tracing::info!(target: "ternvale::agent", len = text.0.len(), "clipboard_get ok");
                Ok(text.0)
            }
            Ok(Err(reason)) => {
                tracing::warn!(target: "ternvale::agent", reason = %reason, "clipboard_get failed in guest");
                Err(AgentServerError::NotConnected {
                    state: "clipboard_get failed",
                })
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let mut book = self.shared.book();
                book.clipboard_wait = None;
                Err(AgentServerError::ClipboardTimeout {
                    timeout_ms: status::millis(timeout),
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(AgentServerError::NotConnected {
                state: "disconnected during clipboard_get",
            }),
        }
    }
}

impl Drop for AgentServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::error!(target: "ternvale::agent", "agent server thread panicked");
            }
        }
        match std::fs::remove_file(&self.path) {
            Ok(()) => {
                tracing::info!(target: "ternvale::agent", path = %self.path.display(), "agent server stopped")
            }
            Err(error) => {
                tracing::warn!(target: "ternvale::agent", path = %self.path.display(), error = %error, "could not remove agent socket")
            }
        }
    }
}

/// Replace a socket file left by a crashed run; refuse a live one or a non-socket.
fn clear_stale(path: &Path) -> Result<(), AgentServerError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(socket_error(path, error)),
        Ok(meta) if !meta.file_type().is_socket() => Err(socket_error(
            path,
            std::io::Error::new(ErrorKind::AlreadyExists, "path exists and is not a socket"),
        )),
        Ok(_) if UnixStream::connect(path).is_ok() => {
            tracing::error!(target: "ternvale::agent", path = %path.display(), "agent socket already served");
            Err(AgentServerError::InUse {
                path: path.to_path_buf(),
            })
        }
        Ok(_) => {
            tracing::warn!(target: "ternvale::agent", path = %path.display(), "removing stale agent socket");
            std::fs::remove_file(path).map_err(|source| socket_error(path, source))
        }
    }
}

fn socket_error(path: &Path, source: std::io::Error) -> AgentServerError {
    tracing::error!(target: "ternvale::agent", path = %path.display(), error = %source, "agent socket setup failed");
    AgentServerError::Socket {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
