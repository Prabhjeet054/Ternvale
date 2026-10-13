//! One command through the CRB: `START` hands the buffer's first
//! `commandSize` bytes to the backend unmodified, and the response is copied
//! back into the same buffer. An inline backend answers inside the `START`
//! write. A worker backend answers later: `START` reads 1 until then, and the
//! response is collected on the next register access ([`TpmCrb::settle`]).

use std::time::Instant;

use super::TpmCrb;
use crate::backend::{
    error_response, hex, CommandHeader, HEADER_LEN, RC_COMMAND_SIZE, RC_FAILURE, ST_NO_SESSIONS,
};
use crate::redact::bytes_are_public;
use crate::regs::DATA_BUFFER_SIZE;

/// A command a worker backend is running.
#[derive(Debug, Clone, Copy)]
pub(super) struct Pending {
    locality: u8,
    header: Option<CommandHeader>,
    since: Instant,
}

impl TpmCrb {
    /// `START` = 1 from `locality`.
    pub(super) fn start(&mut self, locality: u8) {
        if self.localities.active() != Some(locality) {
            tracing::warn!(
                target: "ternvale::tpm",
                locality,
                active = ?self.localities.active(),
                "tpm start from a locality that is not active ignored"
            );
            return;
        }
        if self.pending.is_some() {
            tracing::warn!(target: "ternvale::tpm", locality, "tpm start while a command is still running ignored");
            return;
        }
        if self.idle {
            tracing::warn!(target: "ternvale::tpm", locality, "tpm start while idle (no cmdReady); running it anyway, as QEMU does");
        }
        let since = Instant::now();
        let header = CommandHeader::parse(&self.buffer);
        let size = header
            .map(|h| h.size as usize)
            .filter(|size| (HEADER_LEN..=DATA_BUFFER_SIZE).contains(size));
        let shown = size.unwrap_or(HEADER_LEN);
        tracing::trace!(
            target: "ternvale::tpm",
            locality,
            len = shown,
            code = %header.map_or("none".into(), |h| format!("{:#x}", h.code)),
            bytes = %self.loggable(header, &self.buffer[..shown]),
            "tpm command"
        );
        let response = match size {
            Some(size) => match self.runner.run(locality, self.buffer[..size].to_vec()) {
                Some(response) => response,
                None => {
                    tracing::debug!(target: "ternvale::tpm", locality, backend = self.runner.name(), "tpm command handed to the backend worker; START reads 1 until it answers");
                    self.pending = Some(Pending {
                        locality,
                        header,
                        since,
                    });
                    return;
                }
            },
            None => {
                tracing::warn!(
                    target: "ternvale::tpm",
                    locality,
                    size = header.map(|h| h.size),
                    max = DATA_BUFFER_SIZE,
                    "tpm commandSize outside 10..=buffer size; answered TPM_RC_COMMAND_SIZE"
                );
                error_response(ST_NO_SESSIONS, RC_COMMAND_SIZE)
            }
        };
        self.complete(locality, header, response, since);
    }

    /// `bytes` for a TRACE line: hex when [`bytes_are_public`] or
    /// [`crate::LOG_SECRETS_ENV`] allows it, else `redacted`.
    fn loggable(&self, header: Option<CommandHeader>, bytes: &[u8]) -> String {
        if self.log_secrets || bytes_are_public(header) {
            hex(bytes)
        } else {
            "redacted".to_string()
        }
    }

    /// Collect a worker's response if it has arrived.
    pub(super) fn settle(&mut self) {
        let Some(pending) = self.pending else {
            return;
        };
        let Some(response) = self.runner.poll() else {
            return;
        };
        self.pending = None;
        tracing::debug!(
            target: "ternvale::tpm",
            locality = pending.locality,
            elapsed_us = pending.since.elapsed().as_micros() as u64,
            "tpm backend worker answered"
        );
        self.complete(pending.locality, pending.header, response, pending.since);
    }

    /// Check `response`, record the exchange, put the response in the
    /// buffer, and log it.
    fn complete(
        &mut self,
        locality: u8,
        header: Option<CommandHeader>,
        mut response: Vec<u8>,
        since: Instant,
    ) {
        let declared = response
            .get(2..6)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
        if !(HEADER_LEN..=DATA_BUFFER_SIZE).contains(&response.len())
            || declared != Some(response.len())
        {
            tracing::error!(
                target: "ternvale::tpm",
                backend = self.runner.name(),
                len = response.len(),
                declared = ?declared,
                "tpm backend returned a malformed response; answered TPM_RC_FAILURE"
            );
            response = error_response(ST_NO_SESSIONS, RC_FAILURE);
        }
        let ran = header
            .map(|h| h.size as usize)
            .filter(|size| (HEADER_LEN..=DATA_BUFFER_SIZE).contains(size))
            .unwrap_or(HEADER_LEN);
        self.history.record(
            locality,
            header,
            &self.buffer[..ran],
            &response,
            since.elapsed().as_micros() as u64,
        );
        self.buffer[..response.len()].copy_from_slice(&response);
        self.commands += 1;
        let code = header.map_or("none".into(), |h| format!("{:#x}", h.code));
        let rc = format!(
            "{:#x}",
            u32::from_be_bytes([response[6], response[7], response[8], response[9]])
        );
        if self.commands == 1 {
            tracing::info!(target: "ternvale::tpm", locality, code = %code, rc = %rc, backend = self.runner.name(), "tpm first command from the guest");
        }
        tracing::trace!(
            target: "ternvale::tpm",
            locality,
            len = response.len(),
            bytes = %self.loggable(header, &response),
            "tpm response"
        );
        tracing::debug!(
            target: "ternvale::tpm",
            locality,
            code = %code,
            rc = %rc,
            response_len = response.len(),
            commands = self.commands,
            "tpm command completed"
        );
    }
}
