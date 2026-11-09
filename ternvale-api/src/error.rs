//! Typed JSON-RPC and handshake errors.

use thiserror::Error;

/// Failures that map onto JSON-RPC 2.0 error objects.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// JSON-RPC parse error (`-32700`).
    #[error("parse error: {0}")]
    Parse(String),
    /// JSON-RPC invalid request (`-32600`).
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// Unknown method (`-32601`).
    #[error("method not found: {0}")]
    MethodNotFound(String),
    /// Bad params (`-32602`).
    #[error("invalid params: {0}")]
    InvalidParams(String),
    /// Handshake `api_version` is outside `[min, max]`.
    #[error("unsupported api_version {got} (supported {min}..={max})")]
    UnsupportedVersion { got: u32, min: u32, max: u32 },
    /// Application error (`-32000` and below).
    #[error("{0}")]
    Application(String),
}

impl ApiError {
    /// JSON-RPC `error.code`.
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
    pub fn code(&self) -> i64 {
        match self {
            Self::Parse(_) => -32700,
            Self::InvalidRequest(_) => -32600,
            Self::MethodNotFound(_) => -32601,
            Self::InvalidParams(_) => -32602,
            Self::UnsupportedVersion { .. } => -32001,
            Self::Application(_) => -32000,
        }
    }
}
