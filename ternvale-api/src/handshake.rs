//! Explicit `api_version` handshake (JSON-RPC method `handshake`).

use serde::{Deserialize, Serialize};

/// Current control API version. Additive optional fields do not bump this.
pub const API_VERSION: u32 = 1;
/// Oldest client this server still accepts.
pub const MIN_API_VERSION: u32 = 1;
/// Newest protocol this process speaks.
pub const MAX_API_VERSION: u32 = 1;

/// `handshake` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeParams {
    /// Client's requested version.
    pub api_version: u32,
}

/// `handshake` result. Optional extra fields may be added later.
/// Unknown JSON keys are ignored so an `api_version=1` client keeps working.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandshakeResult {
    pub ok: bool,
    pub api_version: u32,
    pub min_api_version: u32,
    pub max_api_version: u32,
    /// Resolved hypervisor backend (`native` / `vz`). Omitted by [`HandshakeResult::negotiate`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Capability flags for `backend`. Omitted by [`HandshakeResult::negotiate`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<serde_json::Value>,
}

impl HandshakeResult {
    /// Accept `requested` when it is in `[MIN, MAX]`.
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(requested))]
    pub fn negotiate(requested: u32) -> Result<Self, super::ApiError> {
        if requested < MIN_API_VERSION || requested > MAX_API_VERSION {
            tracing::warn!(
                target: "ternvale::api",
                requested,
                min = MIN_API_VERSION,
                max = MAX_API_VERSION,
                "handshake rejected"
            );
            return Err(super::ApiError::UnsupportedVersion {
                got: requested,
                min: MIN_API_VERSION,
                max: MAX_API_VERSION,
            });
        }
        tracing::info!(
            target: "ternvale::api",
            api_version = requested,
            "handshake accepted"
        );
        Ok(Self {
            ok: true,
            api_version: requested,
            min_api_version: MIN_API_VERSION,
            max_api_version: MAX_API_VERSION,
            backend: None,
            capabilities: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::HandshakeResult;
    use serde::Deserialize;

    /// v1 client: known fields only (no `deny_unknown_fields`).
    #[derive(Debug, Deserialize)]
    struct V1Handshake {
        ok: bool,
        api_version: u32,
        min_api_version: u32,
        max_api_version: u32,
    }

    #[test]
    fn v1_client_ignores_new_optional_server_fields() {
        let server = r#"{"ok":true,"api_version":1,"min_api_version":1,"max_api_version":1,"build":"step83-test","features":["subscribe"]}"#;
        let v1: V1Handshake = serde_json::from_str(server).expect("old client");
        assert!(v1.ok);
        assert_eq!(v1.api_version, 1);
        assert_eq!(v1.min_api_version, 1);
        assert_eq!(v1.max_api_version, 1);
        let current: HandshakeResult = serde_json::from_str(server).expect("current client");
        assert_eq!(current, HandshakeResult::negotiate(1).expect("neg"));
    }
}
