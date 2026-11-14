//! JSON-RPC 2.0 on a per-VM control socket.

use std::path::Path;

use serde_json::Value;
use ternvale_api::{
    is_jsonrpc_line, parse_request, ApiError, EventKind, HandshakeParams, HandshakeResult,
    JsonRpcResponse, Method, JSONRPC_VERSION,
};
use ternvale_devices::AgentServer;
use ternvale_vmm::VmControl;

use crate::diag::DiagSink;
use crate::protocol::{Request, Response};
use crate::server::handle_line_with;

/// What to do after parsing one line.
pub enum Dispatch {
    /// Write this JSON (no trailing newline) and keep reading.
    Reply(String),
    /// Write `reply`, then stream events of `kinds` until the client disconnects.
    Subscribe {
        reply: String,
        kinds: Vec<EventKind>,
    },
}

/// Parse a legacy `cmd` line or a JSON-RPC request.
#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm = control.name()))]
#[allow(clippy::too_many_arguments)]
pub fn dispatch_vm_line(
    line: &str,
    control: &VmControl,
    agent: Option<&AgentServer>,
    diag: Option<&DiagSink>,
    tools_iso: Option<&Path>,
    power: Option<&crate::power::PowerState>,
    allow_exec: bool,
    backend: &str,
    capabilities: ternvale_backend::BackendCapabilities,
    x86_engine: &str,
    handshook: &mut bool,
) -> Dispatch {
    if !is_jsonrpc_line(line) {
        let response = handle_line_with(
            line,
            control,
            agent,
            diag,
            tools_iso,
            power,
            allow_exec,
            backend,
            capabilities,
            x86_engine,
        );
        return encode_legacy(&response);
    }
    let request = match parse_request(line) {
        Ok(request) => request,
        Err(error) => {
            return encode_rpc(JsonRpcResponse::from_api_error(Value::Null, &error));
        }
    };
    let id = request.id.clone().unwrap_or(Value::Null);
    if request.method == "event" {
        let err = ApiError::InvalidRequest("event is a notification, not a call".into());
        return encode_rpc(JsonRpcResponse::from_api_error(id, &err));
    }
    let Some(method) = Method::parse(&request.method) else {
        return encode_rpc(JsonRpcResponse::from_api_error(
            id,
            &ApiError::MethodNotFound(request.method.clone()),
        ));
    };
    if method != Method::Handshake && !*handshook {
        tracing::warn!(
            target: "ternvale::api",
            method = method.as_str(),
            "json-rpc call before handshake"
        );
        return encode_rpc(JsonRpcResponse::from_api_error(
            id,
            &ApiError::InvalidRequest("handshake required".into()),
        ));
    }
    match method {
        Method::Handshake => match handshake_result(request.params.as_ref()) {
            Ok(result) => {
                *handshook = true;
                encode_rpc(JsonRpcResponse::result(
                    id,
                    serde_json::to_value(result).unwrap_or(Value::Null),
                ))
            }
            Err(error) => encode_rpc(JsonRpcResponse::from_api_error(id, &error)),
        },
        Method::Subscribe => {
            let kinds = subscribe_kinds(request.params.as_ref());
            let names: Vec<String> = kinds.iter().map(|k| k.as_str().to_string()).collect();
            let reply = JsonRpcResponse::result(
                id,
                serde_json::to_value(Response::subscribed(names)).unwrap_or(Value::Null),
            );
            Dispatch::Subscribe {
                reply: encode_value(&reply),
                kinds,
            }
        }
        _ => {
            let cmd = match jsonrpc_to_request(method, request.params.as_ref()) {
                Ok(cmd) => cmd,
                Err(error) => return encode_rpc(JsonRpcResponse::from_api_error(id, &error)),
            };
            let line = match serde_json::to_string(&cmd) {
                Ok(line) => line,
                Err(error) => {
                    return encode_rpc(JsonRpcResponse::from_api_error(
                        id,
                        &ApiError::Application(error.to_string()),
                    ));
                }
            };
            let response = handle_line_with(
                &line,
                control,
                agent,
                diag,
                tools_iso,
                power,
                allow_exec,
                backend,
                capabilities,
                x86_engine,
            );
            encode_rpc(JsonRpcResponse::result(
                id,
                serde_json::to_value(&response).unwrap_or(Value::Null),
            ))
        }
    }
}

fn handshake_result(params: Option<&Value>) -> Result<HandshakeResult, ApiError> {
    let params: HandshakeParams = match params {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| ApiError::InvalidParams(error.to_string()))?,
        None => return Err(ApiError::InvalidParams("api_version required".into())),
    };
    attach_backend(HandshakeResult::negotiate(params.api_version)?)
}

#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
fn attach_backend(mut result: HandshakeResult) -> Result<HandshakeResult, ApiError> {
    result.backend = Some("native".into());
    match serde_json::to_value(ternvale_backend::BackendCapabilities::native()) {
        Ok(value) => result.capabilities = Some(value),
        Err(error) => {
            tracing::error!(
                target: "ternvale::api",
                error = %error,
                "failed to encode backend capabilities"
            );
            return Err(ApiError::Application(error.to_string()));
        }
    }
    Ok(result)
}

fn subscribe_kinds(params: Option<&Value>) -> Vec<EventKind> {
    let Some(Value::Object(map)) = params else {
        return EventKind::all_default();
    };
    let Some(Value::Array(list)) = map.get("events") else {
        return EventKind::all_default();
    };
    let mut kinds = Vec::new();
    for item in list {
        if let Some(name) = item.as_str() {
            if let Some(kind) = EventKind::parse(name) {
                kinds.push(kind);
            }
        }
    }
    if kinds.is_empty() {
        EventKind::all_default()
    } else {
        kinds
    }
}

fn jsonrpc_to_request(method: Method, params: Option<&Value>) -> Result<Request, ApiError> {
    let map = match params {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => return Err(ApiError::InvalidParams("params must be an object".into())),
    };
    let get_str = |key: &str| -> Option<String> {
        map.get(key).and_then(|v| v.as_str()).map(ToOwned::to_owned)
    };
    let timeout = map.get("timeout_ms").and_then(Value::as_u64);
    match method {
        Method::Status => Ok(Request::Status),
        Method::Pause => Ok(Request::Pause {
            timeout_ms: timeout,
        }),
        Method::Resume => Ok(Request::Resume),
        Method::Stop => {
            if map.get("force").and_then(Value::as_bool) == Some(true) {
                Ok(Request::ForceStop)
            } else if map.get("graceful").and_then(Value::as_bool) == Some(true) {
                Ok(Request::GracefulStop {
                    timeout_ms: timeout,
                })
            } else {
                Ok(Request::Shutdown)
            }
        }
        Method::Stats => Ok(Request::QueryStats),
        Method::Balloon => Ok(Request::BalloonSet {
            target_mib: map.get("target_mib").and_then(Value::as_u64),
            auto: map.get("auto").and_then(Value::as_bool),
        }),
        Method::SnapshotCreate => Ok(Request::SnapshotCreate {
            name: get_str("snapshot").ok_or_else(|| ApiError::InvalidParams("snapshot".into()))?,
        }),
        Method::SnapshotRestore => Ok(Request::SnapshotRestore {
            name: get_str("snapshot").ok_or_else(|| ApiError::InvalidParams("snapshot".into()))?,
        }),
        Method::SnapshotList => Ok(Request::SnapshotList),
        Method::SnapshotDelete => Ok(Request::SnapshotDelete {
            name: get_str("snapshot").ok_or_else(|| ApiError::InvalidParams("snapshot".into()))?,
        }),
        Method::Handshake | Method::Subscribe => Err(ApiError::InvalidRequest("internal".into())),
        other => Err(ApiError::MethodNotFound(format!(
            "{} is a hub method (connect to api.sock)",
            other.as_str()
        ))),
    }
}

fn encode_legacy(response: &Response) -> Dispatch {
    Dispatch::Reply(
        serde_json::to_string(response)
            .unwrap_or_else(|_| r#"{"ok":false,"error":"encode failed"}"#.to_string()),
    )
}

fn encode_rpc(response: JsonRpcResponse) -> Dispatch {
    Dispatch::Reply(encode_value(&response))
}

fn encode_value<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| {
        format!(
            r#"{{"jsonrpc":"{JSONRPC_VERSION}","error":{{"code":-32603,"message":"encode failed"}},"id":null}}"#
        )
    })
}

trait EventKindExt {
    fn all_default() -> Vec<EventKind>;
}

impl EventKindExt for EventKind {
    fn all_default() -> Vec<EventKind> {
        ternvale_api::EVENT_KINDS.to_vec()
    }
}
