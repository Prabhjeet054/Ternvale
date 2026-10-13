//! [`SwtpmBackend`]: TPM commands forwarded to swtpm unmodified.
//!
//! Start-up follows QEMU's `tpm_emulator`: `CMD_GET_CAPABILITY` (check the
//! commands used below), `CMD_SET_BUFFERSIZE` with the CRB buffer size, then
//! `CMD_INIT`. The buffer size must be set before `CMD_INIT`; afterwards
//! swtpm reports `TPM_PT_MAX_COMMAND_SIZE` and `TPM_PT_MAX_RESPONSE_SIZE` as
//! that size (3968, checked with swtpm 0.10.2), so a guest never gets a
//! response larger than the CRB buffer. The guest's own `TPM2_Startup`
//! follows on the data channel.
//!
//! Per command: `CMD_SET_LOCALITY` when the locality differs from the last
//! one, the command bytes on the data socket, then a 10-byte response header
//! and `responseSize - 10` more bytes back. Any I/O or framing failure puts
//! the backend in failure mode: this and every later command get
//! `TPM_RC_FAILURE`, as a TPM in failure mode would answer.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use super::ctrl::{Control, REQUIRED_CAPS};
use super::locate::{find_swtpm, SwtpmInfo};
use super::process::SwtpmProcess;
use super::state::{check_state_file, StateFile};
use super::SwtpmError;
use crate::backend::{error_response, TpmBackend, HEADER_LEN, RC_FAILURE, ST_NO_SESSIONS};
use crate::regs::DATA_BUFFER_SIZE;

/// Longest a command may take. libtpms RSA key generation can take seconds.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(180);
/// Largest response accepted from swtpm before the CRB's own size check.
const MAX_RESPONSE: usize = 64 * 1024;

/// A running swtpm the CRB forwards commands to.
#[derive(Debug)]
pub struct SwtpmBackend {
    info: Option<SwtpmInfo>,
    ctrl: Control,
    data: UnixStream,
    process: Option<SwtpmProcess>,
    locality: Option<u8>,
    failed: bool,
    commands: u64,
}

impl SwtpmBackend {
    /// Find swtpm (`binary`, else `PATH` and the Homebrew directories),
    /// check `state_file`, spawn swtpm on it, and start the TPM.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(state = %state_file.display()))]
    pub fn start(binary: Option<&Path>, state_file: &Path) -> Result<Self, SwtpmError> {
        let info = find_swtpm(binary)?;
        let state = check_state_file(state_file)?;
        let (process, ctrl, data) = SwtpmProcess::spawn(&info.path, state_file)?;
        let pid = process.pid();
        let mut backend = Self::from_parts(Control::new(ctrl)?, data, Some(process))?;
        if let Err(error) = backend.initialize(DATA_BUFFER_SIZE as u32) {
            // e.g. a state file locked by another swtpm: CMD_INIT only says
            // "result 0x9", swtpm's stderr says why.
            let output = backend.process.take().map(SwtpmProcess::stop);
            let error = match (error, output) {
                (SwtpmError::Control { command, reason }, Some(output)) => SwtpmError::Control {
                    command,
                    reason: format!("{reason}; swtpm said: {output}"),
                },
                (error, _) => error,
            };
            tracing::error!(target: "ternvale::tpm", error = %error, "swtpm start-up failed");
            return Err(error);
        }
        tracing::info!(
            target: "ternvale::tpm",
            binary = %info.path.display(),
            version = %info.version,
            pid,
            state = %state_file.display(),
            new_tpm = state == StateFile::New,
            "swtpm ready"
        );
        backend.info = Some(info);
        Ok(backend)
    }

    /// A backend over already-connected sockets (and the process behind
    /// them, if Ternvale started it).
    pub(super) fn from_parts(
        ctrl: Control,
        data: UnixStream,
        process: Option<SwtpmProcess>,
    ) -> Result<Self, SwtpmError> {
        data.set_read_timeout(Some(COMMAND_TIMEOUT))
            .and_then(|()| data.set_write_timeout(Some(COMMAND_TIMEOUT)))
            .map_err(|error| SwtpmError::Data {
                reason: format!("socket setup: {error}"),
            })?;
        Ok(Self {
            info: None,
            ctrl,
            data,
            process,
            locality: None,
            failed: false,
            commands: 0,
        })
    }

    /// The binary this backend runs, when it started one.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn info(&self) -> Option<&SwtpmInfo> {
        self.info.as_ref()
    }

    /// Capabilities, buffer size, `CMD_INIT`.
    pub(super) fn initialize(&mut self, buffer_size: u32) -> Result<(), SwtpmError> {
        let caps = self.ctrl.capabilities()?;
        if caps & REQUIRED_CAPS != REQUIRED_CAPS {
            return Err(SwtpmError::Control {
                command: "CMD_GET_CAPABILITY",
                reason: format!("capabilities {caps:#x} lack {:#x}", REQUIRED_CAPS & !caps),
            });
        }
        let (in_use, min, max) = self.ctrl.set_buffer_size(buffer_size)?;
        if in_use > buffer_size {
            return Err(SwtpmError::Control {
                command: "CMD_SET_BUFFERSIZE",
                reason: format!(
                    "asked for {buffer_size} bytes, swtpm uses {in_use} (it supports {min}..={max})"
                ),
            });
        }
        self.ctrl.init()?;
        tracing::debug!(target: "ternvale::tpm", caps = %format!("{caps:#x}"), buffer = in_use, min, max, "swtpm tpm initialized");
        Ok(())
    }

    /// One command and its response.
    fn exchange(&mut self, locality: u8, command: &[u8]) -> Result<Vec<u8>, SwtpmError> {
        if self.locality != Some(locality) {
            self.ctrl.set_locality(locality)?;
            tracing::debug!(target: "ternvale::tpm", from = ?self.locality, to = locality, "swtpm locality set");
            self.locality = Some(locality);
        }
        let data_error = |what: &str, error: std::io::Error| SwtpmError::Data {
            reason: format!("{what}: {error}"),
        };
        self.data
            .write_all(command)
            .map_err(|error| data_error("write command", error))?;
        let mut response = vec![0; HEADER_LEN];
        self.data
            .read_exact(&mut response)
            .map_err(|error| data_error("read response header", error))?;
        let size =
            u32::from_be_bytes([response[2], response[3], response[4], response[5]]) as usize;
        if !(HEADER_LEN..=MAX_RESPONSE).contains(&size) {
            return Err(SwtpmError::Data {
                reason: format!("responseSize {size} outside {HEADER_LEN}..={MAX_RESPONSE}"),
            });
        }
        response.resize(size, 0);
        self.data
            .read_exact(&mut response[HEADER_LEN..])
            .map_err(|error| data_error("read response body", error))?;
        Ok(response)
    }
}

impl TpmBackend for SwtpmBackend {
    fn name(&self) -> &str {
        "swtpm"
    }

    fn execute(&mut self, locality: u8, command: &[u8]) -> Vec<u8> {
        if self.failed {
            tracing::debug!(target: "ternvale::tpm", locality, "swtpm backend is in failure mode; answered TPM_RC_FAILURE");
            return error_response(ST_NO_SESSIONS, RC_FAILURE);
        }
        match self.exchange(locality, command) {
            Ok(response) => {
                self.commands += 1;
                tracing::debug!(target: "ternvale::tpm", locality, len = response.len(), commands = self.commands, "swtpm answered");
                response
            }
            Err(error) => {
                self.failed = true;
                let output = self.process.as_ref().map(SwtpmProcess::recent_output);
                tracing::error!(target: "ternvale::tpm", locality, error = %error, swtpm_output = ?output, "swtpm exchange failed; every further command gets TPM_RC_FAILURE");
                error_response(ST_NO_SESSIONS, RC_FAILURE)
            }
        }
    }

    fn host_io(&self) -> bool {
        true
    }
}

impl Drop for SwtpmBackend {
    fn drop(&mut self) {
        let Some(mut process) = self.process.take() else {
            return;
        };
        match self.ctrl.shutdown() {
            Ok(()) => {
                process.exiting();
                tracing::debug!(target: "ternvale::tpm", commands = self.commands, "swtpm CMD_SHUTDOWN sent");
            }
            Err(error) => {
                tracing::warn!(target: "ternvale::tpm", error = %error, "swtpm CMD_SHUTDOWN failed; killing it");
            }
        }
        drop(process);
    }
}
