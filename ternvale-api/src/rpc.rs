//! JSON-RPC 2.0 envelope (RFC 2026-style, one object per line).

use serde::{Deserialize, Serialize};

use crate::ApiError;

/// JSON-RPC version string. Frozen.
pub const JSONRPC_VERSION: &str = "2.0";

/// Request or notification `id` (string, number, or null).
pub type JsonRpcId = serde_json::Value;

/// One JSON-RPC request or notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<JsonRpcId>,
}

/// JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// One JSON-RPC response (`result` XOR `error`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: JsonRpcId,
}

impl JsonRpcResponse {
    /// Success.
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
    pub fn result(id: JsonRpcId, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }

    /// Failure.
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(code = error.code()))]
    pub fn from_api_error(id: JsonRpcId, error: &ApiError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            result: None,
            error: Some(JsonRpcError {
                code: error.code(),
                message: error.to_string(),
                data: None,
            }),
            id,
        }
    }
}

/// True when `line` is a JSON object that names `jsonrpc`.
#[tracing::instrument(level = "trace", target = "ternvale::api", skip_all)]
pub fn is_jsonrpc_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(serde_json::Value::Object(map)) => map.contains_key("jsonrpc"),
        _ => false,
    }
}

/// Parse a JSON-RPC request; returns `ApiError` on envelope problems.
#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
pub fn parse_request(line: &str) -> Result<JsonRpcRequest, ApiError> {
    let request: JsonRpcRequest = serde_json::from_str(line).map_err(|error| {
        tracing::warn!(target: "ternvale::api", error = %error, "json-rpc parse error");
        ApiError::Parse(error.to_string())
    })?;
    if request.jsonrpc != JSONRPC_VERSION {
        return Err(ApiError::InvalidRequest(format!(
            "jsonrpc must be {JSONRPC_VERSION}"
        )));
    }
    if request.method.is_empty() {
        return Err(ApiError::InvalidRequest("method is empty".into()));
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_jsonrpc_vs_legacy_cmd() {
        assert!(is_jsonrpc_line(
            r#"{"jsonrpc":"2.0","method":"list","id":1}"#
        ));
        assert!(!is_jsonrpc_line(r#"{"cmd":"status"}"#));
        assert!(!is_jsonrpc_line("status"));
    }
}
