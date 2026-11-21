//! Gateway errors. Never include bearer secrets or private keys.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Failure of TLS, HTTP, auth, or the hub proxy.
#[derive(Debug, Error)]
pub enum GatewayError {
    /// Config or bind policy refused the listen address.
    #[error("{0}")]
    Config(String),
    /// Unknown persisted format version.
    #[error("{0}")]
    Version(String),
    /// I/O on certs, tokens, audit log, or sockets.
    #[error("{op}: {source}")]
    Io {
        /// What was being done.
        op: &'static str,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// Path-related I/O with a display path.
    #[error("{op} {path}: {source}")]
    IoPath {
        /// What was being done.
        op: &'static str,
        /// File involved.
        path: String,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// TLS handshake or configuration.
    #[error("tls: {0}")]
    Tls(String),
    /// Certificate generation.
    #[error("certificate: {0}")]
    Cert(String),
    /// Hub JSON-RPC.
    #[error("hub: {0}")]
    Hub(String),
    /// HTTP request rejected before the hub.
    #[error("{0}")]
    Http(String),
    /// JSON body.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// TOML config.
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    /// RNG failed while issuing a token.
    #[error("random number generator failed")]
    Rng,
    /// Missing state directory file.
    #[error("missing {0}")]
    Missing(PathBuf),
}

impl GatewayError {
    /// Wrap `source` from `op`.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
    pub fn io(op: &'static str, source: io::Error) -> Self {
        Self::Io { op, source }
    }

    /// Wrap `source` from `op` on `path`.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
    pub fn io_path(op: &'static str, path: impl std::fmt::Display, source: io::Error) -> Self {
        Self::IoPath {
            op,
            path: path.to_string(),
            source,
        }
    }
}
