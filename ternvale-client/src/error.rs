//! Client errors.

use ternvale_api::ApiError;
use thiserror::Error;

/// Failures talking to a control socket.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("connect {path}: {source}")]
    Connect {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("read json-rpc: {0}")]
    Read(#[from] std::io::Error),
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("handshake required before {0}")]
    NeedHandshake(&'static str),
    #[error("server error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("empty json-rpc response")]
    Empty,
    #[error("{0}")]
    Failed(String),
}
