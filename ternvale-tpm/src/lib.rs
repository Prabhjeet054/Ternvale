//! TPM 2.0 for Ternvale guests: the Command Response Buffer (CRB) interface.
//!
//! [`TpmCrb`] models the CRB MMIO window of the TCG PC Client Platform TPM
//! Profile (PTP) specification: five locality pages, each with the locality
//! registers, the control area (start/cancel/request/status) and the shared
//! command/response buffer ([`regs`] has the map). A command written to the
//! buffer runs when the active locality sets `START`, on a [`TpmBackend`]:
//! [`SwtpmBackend`] forwards it unmodified to a spawned swtpm (on a worker
//! thread, so the vCPU never waits on the socket); [`StubBackend`] answers
//! every command with an error code. Every command and response is logged at
//! TRACE on `ternvale::tpm`, with bytes only for commands [`redact`] deems
//! public, and the latest ones are kept in a [`TpmHistory`] for crash reports.
//!
//! The crate does not depend on the VMM: `ternvale-vmm` puts a [`TpmCrb`] on
//! its MMIO bus and describes it to the guest with the ACPI TPM2 table and
//! the DSDT `\_SB.TPM2` device (`ternvale-acpi`).

mod backend;
mod crb;
mod history;
mod locality;
pub mod redact;
pub mod regs;
mod swtpm;
#[cfg(test)]
mod test_log;
mod worker;

pub use backend::{
    error_response, header_error, hex, CommandHeader, StubBackend, TpmBackend, HEADER_LEN,
    RC_BAD_TAG, RC_COMMAND_CODE, RC_COMMAND_SIZE, RC_FAILURE, ST_NO_SESSIONS, ST_RSP_COMMAND,
    ST_SESSIONS,
};
pub use crb::{TpmCrb, LOG_SECRETS_ENV};
pub use history::{
    BackendFailure, TpmExchange, TpmHistory, BACKEND_FAILED, TPM_HISTORY_BYTES, TPM_HISTORY_LEN,
};
pub use locality::Localities;
pub use regs::{CONTROL_AREA_OFFSET, CRB_WINDOW_SIZE, DATA_BUFFER, DATA_BUFFER_SIZE, LOCALITIES};
pub use swtpm::{
    check_state_file, find_swtpm, locate_swtpm, parse_swtpm_version, probe_swtpm, StateFile,
    SwtpmBackend, SwtpmError, SwtpmInfo, REQUIRED_FEATURES, STATE_MAGIC, STATE_VERSION,
    SWTPM_FALLBACK_DIRS, TESTED_SWTPM_VERSION,
};

/// Building a CRB failed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TpmError {
    /// The window base is not 4 KiB aligned.
    #[error("tpm crb base {base:#x} is not 4 KiB aligned")]
    Misaligned {
        /// Rejected base.
        base: u64,
    },
    /// The window would run past the end of the address space.
    #[error("tpm crb window at {base:#x} overflows")]
    Overflow {
        /// Rejected base.
        base: u64,
    },
    /// The backend's worker thread could not be started.
    #[error("start the tpm backend worker: {reason}")]
    Worker {
        /// Why.
        reason: String,
    },
}
