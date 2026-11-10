//! Host-side `Exec` / copy waiters.

use std::sync::mpsc;
use std::time::Duration;

use ternvale_agent_proto::{ExecStream, HexBytes, Message, MAX_BIN_CHUNK};

use super::status::millis;
use super::{AgentServer, AgentServerError};

/// One event from a guest `Exec`.
#[derive(Debug, Clone)]
pub enum ExecEvent {
    /// stdout or stderr bytes.
    Output {
        /// Stream.
        stream: ExecStream,
        /// Chunk.
        data: Vec<u8>,
    },
    /// Process ended.
    Exit {
        /// Exit status.
        code: i32,
    },
    /// Guest reported failure before an exit.
    Failed {
        /// Reason.
        reason: String,
    },
}

/// One reply to a copy request.
#[derive(Debug, Clone)]
pub enum CopyEvent {
    /// `CopyPutOk`.
    PutOk {
        /// Bytes written.
        written: u64,
    },
    /// `CopyGetOk`.
    GetOk {
        /// Chunk.
        data: Vec<u8>,
        /// End of file.
        eof: bool,
    },
    /// `CopyHashOk`.
    HashOk {
        /// File size.
        size: u64,
        /// Lowercase hex SHA-256.
        sha256: String,
    },
    /// `CopyErr`.
    Failed {
        /// Reason.
        reason: String,
    },
}

impl AgentServer {
    /// Run `argv` in the guest. `on_output` is called for each stdout/stderr chunk.
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(vm = %self.shared.vm, argc = argv.len()))]
    pub fn exec(
        &self,
        argv: &[String],
        timeout: Duration,
        mut on_output: impl FnMut(ExecStream, &[u8]),
    ) -> Result<i32, AgentServerError> {
        tracing::info!(
            target: "ternvale::agent",
            argv = ?argv,
            timeout_ms = millis(timeout),
            "exec request"
        );
        let id = self.next_id();
        let (tx, rx) = mpsc::channel();
        {
            let mut book = self.shared.book();
            if book.exec_wait.is_some() {
                return Err(AgentServerError::NotConnected {
                    state: "exec already pending",
                });
            }
            book.exec_wait = Some((id, tx));
        }
        self.send(&Message::Exec {
            id,
            argv: argv.to_vec(),
            timeout_ms: Some(millis(timeout)),
        })?;
        // Guest kills at `timeout` and replies 124; wait a little longer for ExecExit.
        let host_wait = timeout.saturating_add(Duration::from_secs(2));
        loop {
            match rx.recv_timeout(host_wait) {
                Ok(ExecEvent::Output { stream, data }) => on_output(stream, &data),
                Ok(ExecEvent::Exit { code }) => {
                    tracing::info!(target: "ternvale::agent", id, code, "exec finished");
                    return Ok(code);
                }
                Ok(ExecEvent::Failed { reason }) => {
                    self.clear_exec();
                    tracing::warn!(target: "ternvale::agent", id, reason = %reason, "exec failed");
                    return Err(AgentServerError::NotConnected {
                        state: "exec failed",
                    });
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.clear_exec();
                    return Err(AgentServerError::ExecTimeout {
                        timeout_ms: millis(timeout),
                    });
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(AgentServerError::NotConnected {
                        state: "disconnected during exec",
                    });
                }
            }
        }
    }

    /// Write `data` at `offset` in the guest file (`last` finishes the write).
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(vm = %self.shared.vm, offset, n = data.len(), last))]
    pub fn copy_put(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
        last: bool,
        timeout: Duration,
    ) -> Result<u64, AgentServerError> {
        if data.len() > MAX_BIN_CHUNK {
            return Err(AgentServerError::NotARequest { kind: "copy_put" });
        }
        match self.copy_rpc(
            Message::CopyPut {
                id: 0,
                path: path.to_string(),
                offset,
                data: HexBytes(data.to_vec()),
                last,
            },
            timeout,
        )? {
            CopyEvent::PutOk { written } => Ok(written),
            CopyEvent::Failed { reason } => {
                tracing::warn!(target: "ternvale::agent", path, reason = %reason, "copy_put failed");
                Err(AgentServerError::NotConnected {
                    state: "copy_put failed",
                })
            }
            _ => Err(AgentServerError::NotConnected {
                state: "unexpected copy_put reply",
            }),
        }
    }

    /// Read up to `len` bytes from the guest file.
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(vm = %self.shared.vm, offset, len))]
    pub fn copy_get(
        &self,
        path: &str,
        offset: u64,
        len: u32,
        timeout: Duration,
    ) -> Result<(Vec<u8>, bool), AgentServerError> {
        match self.copy_rpc(
            Message::CopyGet {
                id: 0,
                path: path.to_string(),
                offset,
                len,
            },
            timeout,
        )? {
            CopyEvent::GetOk { data, eof } => Ok((data, eof)),
            CopyEvent::Failed { reason } => {
                tracing::warn!(target: "ternvale::agent", path, reason = %reason, "copy_get failed");
                Err(AgentServerError::NotConnected {
                    state: "copy_get failed",
                })
            }
            _ => Err(AgentServerError::NotConnected {
                state: "unexpected copy_get reply",
            }),
        }
    }

    /// SHA-256 and size of a guest file.
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(vm = %self.shared.vm))]
    pub fn copy_hash(
        &self,
        path: &str,
        timeout: Duration,
    ) -> Result<(u64, String), AgentServerError> {
        match self.copy_rpc(
            Message::CopyHash {
                id: 0,
                path: path.to_string(),
            },
            timeout,
        )? {
            CopyEvent::HashOk { size, sha256 } => Ok((size, sha256)),
            CopyEvent::Failed { reason } => {
                tracing::warn!(target: "ternvale::agent", path, reason = %reason, "copy_hash failed");
                Err(AgentServerError::NotConnected {
                    state: "copy_hash failed",
                })
            }
            _ => Err(AgentServerError::NotConnected {
                state: "unexpected copy_hash reply",
            }),
        }
    }

    fn copy_rpc(
        &self,
        mut message: Message,
        timeout: Duration,
    ) -> Result<CopyEvent, AgentServerError> {
        let id = self.next_id();
        match &mut message {
            Message::CopyPut { id: slot, .. }
            | Message::CopyGet { id: slot, .. }
            | Message::CopyHash { id: slot, .. } => *slot = id,
            _ => {
                return Err(AgentServerError::NotARequest {
                    kind: message.kind(),
                })
            }
        }
        let (tx, rx) = mpsc::channel();
        {
            let mut book = self.shared.book();
            if book.copy_wait.is_some() {
                return Err(AgentServerError::NotConnected {
                    state: "copy already pending",
                });
            }
            book.copy_wait = Some((id, tx));
        }
        self.send(&message)?;
        match rx.recv_timeout(timeout) {
            Ok(event) => Ok(event),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.clear_copy();
                Err(AgentServerError::CopyTimeout {
                    timeout_ms: millis(timeout),
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(AgentServerError::NotConnected {
                state: "disconnected during copy",
            }),
        }
    }

    fn next_id(&self) -> u64 {
        self.shared
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1
    }

    fn clear_exec(&self) {
        let mut book = self.shared.book();
        book.exec_wait = None;
    }

    fn clear_copy(&self) {
        let mut book = self.shared.book();
        book.copy_wait = None;
    }
}
