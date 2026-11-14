//! Errors from config mapping and the VZ C ABI.

/// A Virtualization.framework host failure.
#[derive(Debug, thiserror::Error)]
pub enum VzError {
    /// Config option the VZ backend cannot honour.
    #[error("{option} is not supported by this backend (capability {capability})")]
    UnsupportedOption {
        /// TOML / config field name.
        option: String,
        /// [`ternvale_backend::BackendCapabilities`] flag, or `none`.
        capability: &'static str,
    },
    /// C ABI / framework call failed.
    #[error("{op}: {detail}")]
    Host {
        /// Operation (`create`, `start`, `pause`, …).
        op: &'static str,
        /// Framework message (no secrets).
        detail: String,
    },
    /// Guest path or serial log could not be prepared.
    #[error("{op}: {detail}")]
    Io {
        /// Operation name.
        op: &'static str,
        /// Display text.
        detail: String,
    },
    /// `rosetta = true` but `VZLinuxRosettaAvailability.notInstalled`.
    #[error(
        "Rosetta for Linux is not installed; run `ternvale doctor --install-rosetta` after accepting Apple's terms (Ternvale never installs Rosetta silently or redistributes it)"
    )]
    RosettaNotInstalled,
    /// Host cannot offer Linux Rosetta (not Apple silicon / too old / VZ-only).
    #[error(
        "Rosetta for Linux is not available on this host (VZ backend only; Apple silicon, macOS 13+)"
    )]
    RosettaUnavailable,
    /// User did not type `yes`; `installRosetta` was not called.
    #[error("Rosetta install declined; nothing was downloaded or installed")]
    RosettaInstallDeclined,
}
