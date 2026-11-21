//! License errors. Never include private keys.

use thiserror::Error;

/// Failure to parse, verify, or persist a license or trial.
#[derive(Debug, Error)]
pub enum LicenseError {
    /// Unknown magic or version.
    #[error("{0}")]
    Version(String),
    /// Signature or payload rejected.
    #[error("{0}")]
    Verify(String),
    /// I/O.
    #[error("{op}: {source}")]
    Io {
        /// Operation.
        op: &'static str,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// JSON.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// RNG (sign tool).
    #[error("random number generator failed")]
    Rng,
    /// New VM sessions are not allowed (degraded).
    #[error("{0}")]
    Degraded(String),
}

impl LicenseError {
    /// Wrap I/O.
    #[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
    pub fn io(op: &'static str, source: std::io::Error) -> Self {
        Self::Io { op, source }
    }
}
