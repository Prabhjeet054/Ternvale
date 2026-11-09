//! Async JSON-RPC 2.0 client for `run/api.sock` and per-VM control sockets.

mod client;
mod error;
pub mod lifecycle;

pub use client::Client;
pub use error::ClientError;
pub use ternvale_api::{
    Event, EventKind, HandshakeResult, Method, API_VERSION, JSONRPC_VERSION, METHODS,
};
