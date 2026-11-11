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
}
