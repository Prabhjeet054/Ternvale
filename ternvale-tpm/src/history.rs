//! The guest's last [`TPM_HISTORY_LEN`] TPM commands, for crash reports.
//!
//! [`TpmCrb`](crate::TpmCrb) records every completed command here. The VMM
//! hands the shared [`TpmHistory`] to its diagnostics, and the CLI writes
//! [`TpmHistory::render`] next to the host log, so a bundle from a failed
//! measured boot shows which command the guest got stuck on and what the
//! TPM answered. Bytes are kept only where [`bytes_are_public`] allows;
//! every other exchange keeps its command code, sizes and response code. A
//! backend that stops answering (swtpm died) says so through
//! [`TpmHistory::note_failure`], and the report names it in its header.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use crate::backend::{hex, CommandHeader};
use crate::redact::{bytes_are_public, command_name};

/// Exchanges a [`TpmHistory`] keeps by default: room for EDK2's measured
/// boot plus an OS's first commands (Linux's resource manager adds context
/// save/load/flush around each one).
pub const TPM_HISTORY_LEN: usize = 256;
/// Bytes of each public command or response kept; the rest is counted.
pub const TPM_HISTORY_BYTES: usize = 256;

/// One completed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TpmExchange {
    /// 1 for the guest's first command.
    pub seq: u64,
    /// Milliseconds from the CRB's creation to the response.
    pub at_ms: u64,
    /// Locality that set `START`.
    pub locality: u8,
    /// Command tag, `None` when the header did not parse.
    pub tag: Option<u16>,
    /// Command code, `None` when the header did not parse.
    pub code: Option<u32>,
    /// `commandSize` from the header (0 without one).
    pub command_len: usize,
    /// Response code.
    pub rc: u32,
    /// Response length.
    pub response_len: usize,
    /// `START` to response.
    pub elapsed_us: u64,
    /// First [`TPM_HISTORY_BYTES`] of the command; `None` when redacted.
    pub command: Option<Vec<u8>>,
    /// First [`TPM_HISTORY_BYTES`] of the response; `None` when redacted.
    pub response: Option<Vec<u8>>,
}

/// The first time the backend stopped answering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendFailure {
    /// Sequence number of the first command the failure answered.
    pub seq: u64,
    /// Milliseconds from the CRB's creation.
    pub at_ms: u64,
    /// What happened, e.g. swtpm's exit status and the I/O error.
    pub reason: String,
}

/// Starts the [`TpmHistory::render`] line for a [`BackendFailure`].
pub const BACKEND_FAILED: &str = "# backend failed: ";

struct Ring {
    entries: VecDeque<TpmExchange>,
    total: u64,
    failure: Option<BackendFailure>,
}

/// A bounded, shareable record of the latest TPM exchanges.
pub struct TpmHistory {
    backend: String,
    started: Instant,
    capacity: usize,
    ring: Mutex<Ring>,
}

impl std::fmt::Debug for TpmHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TpmHistory")
            .field("backend", &self.backend)
            .field("capacity", &self.capacity)
            .field("total", &self.total())
            .finish()
    }
}

impl TpmHistory {
    /// An empty history of at most `capacity` (at least 1) exchanges with
    /// `backend` (its name, for the report header).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::tpm",
        skip_all,
        fields(backend, capacity)
    )]
    pub fn new(backend: &str, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            backend: backend.to_string(),
            started: Instant::now(),
            capacity,
            ring: Mutex::new(Ring {
                entries: VecDeque::with_capacity(capacity),
                total: 0,
                failure: None,
            }),
        }
    }

    fn ring(&self) -> std::sync::MutexGuard<'_, Ring> {
        // A panic while holding the lock leaves a complete ring (every update
        // is one push or pop), so the data is still good for a crash report.
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record one exchange: `command` as run and `response` as returned
    /// (already validated, at least 10 bytes).
    #[tracing::instrument(level = "trace", target = "ternvale::tpm", skip_all, fields(locality, code = ?header.map(|h| h.code)))]
    pub fn record(
        &self,
        locality: u8,
        header: Option<CommandHeader>,
        command: &[u8],
        response: &[u8],
        elapsed_us: u64,
    ) {
        let public = bytes_are_public(header);
        let keep =
            |bytes: &[u8]| public.then(|| bytes[..bytes.len().min(TPM_HISTORY_BYTES)].to_vec());
        let rc = response
            .get(6..10)
            .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        let mut ring = self.ring();
        ring.total += 1;
        let exchange = TpmExchange {
            seq: ring.total,
            at_ms: self.started.elapsed().as_millis() as u64,
            locality,
            tag: header.map(|h| h.tag),
            code: header.map(|h| h.code),
            command_len: header.map_or(0, |h| h.size as usize),
            rc,
            response_len: response.len(),
            elapsed_us,
            command: keep(command),
            response: keep(response),
        };
        if ring.entries.len() == self.capacity {
            ring.entries.pop_front();
        }
        ring.entries.push_back(exchange);
        tracing::trace!(target: "ternvale::tpm", seq = ring.total, public, rc = %format!("{rc:#x}"), "tpm exchange recorded");
    }

    /// The backend stopped answering: the command it is failing now (the
    /// next one recorded) and every later one get `TPM_RC_FAILURE` from
    /// Ternvale, not from the TPM. Only the first failure is kept. `reason`
    /// must not carry command or response bytes.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn note_failure(&self, reason: &str) {
        let mut ring = self.ring();
        if ring.failure.is_some() {
            return;
        }
        let failure = BackendFailure {
            seq: ring.total + 1,
            at_ms: self.started.elapsed().as_millis() as u64,
            reason: reason.to_string(),
        };
        tracing::debug!(target: "ternvale::tpm", seq = failure.seq, at_ms = failure.at_ms, "tpm backend failure recorded in the command history");
        ring.failure = Some(failure);
    }

    /// The first backend failure, if any.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn failure(&self) -> Option<BackendFailure> {
        self.ring().failure.clone()
    }

    /// The kept exchanges, oldest first.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn exchanges(&self) -> Vec<TpmExchange> {
        self.ring().entries.iter().cloned().collect()
    }

    /// Commands recorded since the history was created.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn total(&self) -> u64 {
        self.ring().total
    }

    /// The backend name given to [`TpmHistory::new`].
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn backend(&self) -> &str {
        &self.backend
    }

    /// The report text: a header, then one `key=value` line per exchange,
    /// oldest first.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(backend = %self.backend))]
    pub fn render(&self) -> String {
        let (entries, total, failure) = {
            let ring = self.ring();
            let entries = ring.entries.iter().cloned().collect::<Vec<_>>();
            (entries, ring.total, ring.failure.clone())
        };
        let mut out = format!(
            "# TPM commands: backend {}, {total} since the VM started, the last {} below (oldest first).\n",
            self.backend,
            entries.len()
        );
        if let Some(f) = &failure {
            out.push_str(&format!(
                "{BACKEND_FAILED}{}; seq={} and every later command got TPM_RC_FAILURE (rc=0x101) from Ternvale, not from the TPM (t_ms={})\n",
                f.reason, f.seq, f.at_ms
            ));
        }
        out.push_str(
            "# Bytes are shown only for session-less commands that carry no secrets (startup, self test,\n\
             # capabilities, PCR reads, public areas, flushes); every other command keeps only its code,\n\
             # sizes and response code. t_ms counts from TPM creation; us is START to response.\n",
        );
        if entries.is_empty() {
            out.push_str("# no TPM commands yet\n");
        }
        for e in &entries {
            let code = e.code.map_or("none".to_string(), |c| format!("{c:#x}"));
            let name = e.code.and_then(command_name).unwrap_or("unknown");
            out.push_str(&format!(
                "seq={} t_ms={} locality={} code={code} name={name} cmd_len={} rc={:#x} rsp_len={} us={}",
                e.seq, e.at_ms, e.locality, e.command_len, e.rc, e.response_len, e.elapsed_us
            ));
            match (&e.command, &e.response) {
                (Some(cmd), Some(rsp)) => out.push_str(&format!(
                    " cmd={}{} rsp={}{}",
                    hex(cmd),
                    more(e.command_len, cmd.len()),
                    hex(rsp),
                    more(e.response_len, rsp.len())
                )),
                _ => out.push_str(" bytes=redacted"),
            }
            out.push('\n');
        }
        out
    }
}

/// `(+N more bytes)` when only `kept` of `len` bytes were kept.
fn more(len: usize, kept: usize) -> String {
    if len > kept {
        format!("(+{} more bytes)", len - kept)
    } else {
        String::new()
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
