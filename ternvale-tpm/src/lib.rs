//! TPM 2.0 for Ternvale guests: the Command Response Buffer (CRB) interface.
//!
//! [`TpmCrb`] models the CRB MMIO window of the TCG PC Client Platform TPM
//! Profile (PTP) specification: five locality pages, each with the locality
//! registers, the control area (start/cancel/request/status) and the shared
//! command/response buffer ([`regs`] has the map). A command written to the
//! buffer runs when the active locality sets `START`, on a [`TpmBackend`].
//! [`StubBackend`] implements no TPM command semantics yet: it answers every
//! command with an error code. Every command and response byte stream is
//! logged at TRACE on `ternvale::tpm`.
//!
//! The crate does not depend on the VMM: `ternvale-vmm` puts a [`TpmCrb`] on
//! its MMIO bus and describes it to the guest with the ACPI TPM2 table and
//! the DSDT `\_SB.TPM2` device (`ternvale-acpi`).

mod backend;
mod crb;
mod locality;
pub mod regs;

pub use backend::{
    error_response, header_error, hex, CommandHeader, StubBackend, TpmBackend, HEADER_LEN,
    RC_BAD_TAG, RC_COMMAND_CODE, RC_COMMAND_SIZE, RC_FAILURE, ST_NO_SESSIONS, ST_RSP_COMMAND,
    ST_SESSIONS,
};
pub use crb::TpmCrb;
pub use locality::Localities;
pub use regs::{CONTROL_AREA_OFFSET, CRB_WINDOW_SIZE, DATA_BUFFER, DATA_BUFFER_SIZE, LOCALITIES};

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
}
