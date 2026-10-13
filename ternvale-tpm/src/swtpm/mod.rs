//! swtpm as a [`TpmBackend`](crate::TpmBackend): the CRB's command bytes go
//! to a spawned `swtpm socket --tpm2` process over a Unix domain socket,
//! unmodified, and swtpm's response bytes come back unmodified.
//!
//! - `locate`: find the binary; check `--print-capabilities`.
//! - `state`: check the state file's header before swtpm opens it.
//! - `process`: spawn swtpm with its data and control sockets in a private
//!   runtime directory; stop it on drop.
//! - `ctrl`: the control channel (swtpm `include/swtpm/tpm_ioctl.h`,
//!   "Commands used by the non-CUSE TPMs").
//! - `backend`: [`SwtpmBackend`].
//!
//! `docs/TPM.md` has the exact invocation and what every flag is for.

mod backend;
mod ctrl;
mod locate;
mod process;
mod state;

use std::path::PathBuf;

pub use backend::SwtpmBackend;
pub use locate::{
    find_swtpm, locate_swtpm, probe_swtpm, SwtpmInfo, REQUIRED_FEATURES, SWTPM_FALLBACK_DIRS,
};
pub use state::{check_state_file, StateFile, STATE_MAGIC, STATE_VERSION};

/// swtpm could not be found, started, or talked to.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SwtpmError {
    /// No swtpm binary.
    #[error("swtpm not found (looked for {searched}); install it with `brew install swtpm`")]
    NotFound {
        /// Where it was looked for.
        searched: String,
    },
    /// The binary runs but lacks something Ternvale's invocation needs.
    #[error("{} cannot be used: {reason}", path.display())]
    Unsupported {
        /// The binary.
        path: PathBuf,
        /// What is missing.
        reason: String,
    },
    /// `swtpm socket --print-capabilities` failed or printed something else.
    #[error("{} socket --print-capabilities: {reason}", path.display())]
    Probe {
        /// The binary.
        path: PathBuf,
        /// What went wrong.
        reason: String,
    },
    /// The state file exists but is not a usable swtpm state file.
    #[error("tpm state file {}: {reason}", path.display())]
    State {
        /// The state file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// The state file has a swtpm format version Ternvale does not know.
    #[error("tpm state file {} is swtpm linear-file format version {found}; Ternvale only knows version {supported}, so it will not let swtpm open it", path.display())]
    StateVersion {
        /// The state file.
        path: PathBuf,
        /// Version byte in the file.
        found: u8,
        /// The version Ternvale accepts.
        supported: u8,
    },
    /// A path cannot be written into a swtpm option.
    #[error("{what} {} cannot be passed to swtpm: {reason}", path.display())]
    Path {
        /// Which path.
        what: &'static str,
        /// The path.
        path: PathBuf,
        /// Why.
        reason: &'static str,
    },
    /// Spawning swtpm or connecting to its sockets failed.
    #[error("start swtpm: {reason}")]
    Spawn {
        /// What went wrong, with swtpm's last stderr lines when it exited.
        reason: String,
    },
    /// A control channel command failed.
    #[error("swtpm control {command}: {reason}")]
    Control {
        /// `CMD_*` name.
        command: &'static str,
        /// What went wrong.
        reason: String,
    },
    /// The data channel failed or returned a malformed response.
    #[error("swtpm data channel: {reason}")]
    Data {
        /// What went wrong.
        reason: String,
    },
}

impl SwtpmError {
    /// No usable swtpm on this host: not installed, too old, or the binary
    /// does not run. The `auto` backend falls back to the stub for these and
    /// only these; a state file or runtime failure stops the VM instead.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn is_unavailable(&self) -> bool {
        matches!(
            self,
            Self::NotFound { .. } | Self::Unsupported { .. } | Self::Probe { .. }
        )
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod real_tests;
