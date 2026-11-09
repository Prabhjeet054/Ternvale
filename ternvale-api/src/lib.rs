//! Versioned JSON-RPC 2.0 control protocol (Step 83).
//!
//! Wire format, method names, events, and the JSON Schema live here so
//! `ternvale-cli`, `ternvale-client`, and the generated Swift client share
//! one source of truth. Existing per-VM `{"cmd":…}` lines remain valid
//! (implicit `api_version` 1).

mod error;
mod events;
mod handshake;
mod methods;
mod rpc;
mod swiftgen;

pub use error::ApiError;
pub use events::{Event, EventKind, EVENT_KINDS};
pub use handshake::{
    HandshakeParams, HandshakeResult, API_VERSION, MAX_API_VERSION, MIN_API_VERSION,
};
pub use methods::{Method, METHODS};
pub use rpc::{
    is_jsonrpc_line, parse_request, JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse,
    JSONRPC_VERSION,
};
pub use swiftgen::generate_swift_client;

/// Embedded JSON Schema for the control API (`docs/schema/control-api.json`).
pub const CONTROL_SCHEMA: &str = include_str!("../../docs/schema/control-api.json");

/// Directory of committed golden request/response files (relative to this crate).
pub const GOLDEN_DIR: &str = "goldens";
