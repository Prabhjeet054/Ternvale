//! REST mapping onto Step 83 methods. New methods are not added to JSON-RPC.

use serde_json::{json, Value};
use ternvale_api::Method;

use crate::auth::Scope;
use crate::error::GatewayError;
use crate::http::{path_segments, valid_vm_name, HttpRequest};
use crate::hub::ControlHub;
use crate::schema::validate_params;

/// Outcome of one REST call (not WebSocket).
#[derive(Debug)]
pub enum RouteResult {
    /// JSON body.
    Json {
        /// HTTP status.
        status: u16,
        /// Payload.
        body: Value,
    },
    /// Raw bytes (PNG).
    Bytes {
        /// HTTP status.
        status: u16,
        /// Content-Type.
        content_type: &'static str,
        /// Payload.
        body: Vec<u8>,
        /// Extra headers.
        extra: Vec<(String, String)>,
    },
    /// Switch to WebSocket (`events` or `serial`).
    WebSocket {
        /// Kind.
        kind: WsKind,
        /// VM name for serial.
        name: Option<String>,
    },
}

/// WebSocket endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsKind {
    /// Hub `subscribe` events.
    Events,
    /// Serial log tail.
    Serial,
}

/// Dispatch one HTTP request (auth already done).
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(method = %request.method, path = %request.path))]
pub fn dispatch(
    request: &HttpRequest,
    scope: Scope,
    hub: &dyn ControlHub,
    run_dir: &std::path::Path,
    vms_dir: &std::path::Path,
) -> Result<RouteResult, GatewayError> {
    let segs = path_segments(&request.path);
    match (request.method.as_str(), segs.as_slice()) {
        ("GET", ["v1", "health"]) => Ok(json_ok(json!({"ok": true}))),
        ("GET", ["v1", "vms"]) => {
            need(scope, Scope::ReadOnly)?;
            hub_json(hub, Method::List, json!({}))
        }
        ("POST", ["v1", "vms"]) => {
            need(scope, Scope::Admin)?;
            let params = json_body(request)?;
            validate_params(Method::Create, &params).map_err(|e| GatewayError::Http(e.0))?;
            hub_json(hub, Method::Create, params)
        }
        ("DELETE", ["v1", "vms", name]) => {
            need(scope, Scope::Admin)?;
            check_name(name)?;
            let params = json!({"name": name});
            validate_params(Method::Delete, &params).map_err(|e| GatewayError::Http(e.0))?;
            hub_json(hub, Method::Delete, params)
        }
        ("POST", ["v1", "vms", name, "start"]) => {
            vm_action(scope, Scope::Operator, hub, Method::Start, name, request)
        }
        ("POST", ["v1", "vms", name, "stop"]) => {
            vm_action(scope, Scope::Operator, hub, Method::Stop, name, request)
        }
        ("POST", ["v1", "vms", name, "pause"]) => {
            vm_action(scope, Scope::Operator, hub, Method::Pause, name, request)
        }
        ("POST", ["v1", "vms", name, "resume"]) => {
            vm_action(scope, Scope::Operator, hub, Method::Resume, name, request)
        }
        ("POST", ["v1", "vms", name, "balloon"]) => {
            vm_action(scope, Scope::Operator, hub, Method::Balloon, name, request)
        }
        ("GET", ["v1", "vms", name, "stats"]) => {
            need(scope, Scope::ReadOnly)?;
            check_name(name)?;
            hub_json(hub, Method::Stats, json!({"name": name}))
        }
        ("GET", ["v1", "vms", name, "snapshots"]) => {
            need(scope, Scope::ReadOnly)?;
            check_name(name)?;
            hub_json(hub, Method::SnapshotList, json!({"name": name}))
        }
        ("POST", ["v1", "vms", name, "snapshots"]) => {
            need(scope, Scope::Operator)?;
            check_name(name)?;
            let mut params = json_body(request)?;
            merge_name(&mut params, name);
            validate_params(Method::SnapshotCreate, &params)
                .map_err(|e| GatewayError::Http(e.0))?;
            hub_json(hub, Method::SnapshotCreate, params)
        }
        ("POST", ["v1", "vms", name, "snapshots", snap, "restore"]) => {
            need(scope, Scope::Operator)?;
            check_name(name)?;
            let params = json!({"name": name, "snapshot": snap});
            validate_params(Method::SnapshotRestore, &params)
                .map_err(|e| GatewayError::Http(e.0))?;
            hub_json(hub, Method::SnapshotRestore, params)
        }
        ("DELETE", ["v1", "vms", name, "snapshots", snap]) => {
            need(scope, Scope::Operator)?;
            check_name(name)?;
            let params = json!({"name": name, "snapshot": snap});
            validate_params(Method::SnapshotDelete, &params)
                .map_err(|e| GatewayError::Http(e.0))?;
            hub_json(hub, Method::SnapshotDelete, params)
        }
        ("POST", ["v1", "vms", name, "clone"]) => {
            need(scope, Scope::Admin)?;
            check_name(name)?;
            let mut params = json_body(request)?;
            if let Value::Object(map) = &mut params {
                map.insert("source".into(), json!(name));
            }
            validate_params(Method::Clone, &params).map_err(|e| GatewayError::Http(e.0))?;
            hub_json(hub, Method::Clone, params)
        }
        ("GET", ["v1", "vms", name, "screenshot.png"]) => {
            need(scope, Scope::ReadOnly)?;
            check_name(name)?;
            let path = crate::extras::framebuffer_path(run_dir, name);
            let (png, present) = crate::extras::screenshot_png(&path)?;
            let extra = vec![(
                "X-Ternvale-Framebuffer".into(),
                if present { "present" } else { "placeholder" }.into(),
            )];
            Ok(RouteResult::Bytes {
                status: 200,
                content_type: "image/png",
                body: png,
                extra,
            })
        }
        ("GET", ["v1", "events"]) => {
            need(scope, Scope::ReadOnly)?;
            let _ = vms_dir;
            Ok(RouteResult::WebSocket {
                kind: WsKind::Events,
                name: None,
            })
        }
        ("GET", ["v1", "vms", name, "serial"]) => {
            need(scope, Scope::ReadOnly)?;
            check_name(name)?;
            Ok(RouteResult::WebSocket {
                kind: WsKind::Serial,
                name: Some((*name).to_string()),
            })
        }
        _ => Err(GatewayError::Http("not found".into())),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn vm_action(
    scope: Scope,
    needed: Scope,
    hub: &dyn ControlHub,
    method: Method,
    name: &str,
    request: &HttpRequest,
) -> Result<RouteResult, GatewayError> {
    need(scope, needed)?;
    check_name(name)?;
    let mut params = json_body(request)?;
    merge_name(&mut params, name);
    validate_params(method, &params).map_err(|e| GatewayError::Http(e.0))?;
    hub_json(hub, method, params)
}

fn need(have: Scope, needed: Scope) -> Result<(), GatewayError> {
    if have.allows(needed) {
        Ok(())
    } else {
        Err(GatewayError::Http("forbidden".into()))
    }
}

fn check_name(name: &str) -> Result<(), GatewayError> {
    if valid_vm_name(name) {
        Ok(())
    } else {
        Err(GatewayError::Http("invalid vm name".into()))
    }
}

fn json_body(request: &HttpRequest) -> Result<Value, GatewayError> {
    if request.body.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(&request.body)
        .map_err(|error| GatewayError::Http(format!("json: {error}")))
}

fn merge_name(params: &mut Value, name: &str) {
    if let Value::Object(map) = params {
        map.insert("name".into(), json!(name));
    }
}

fn hub_json(
    hub: &dyn ControlHub,
    method: Method,
    params: Value,
) -> Result<RouteResult, GatewayError> {
    let body = hub.call(method, params)?;
    Ok(json_ok(body))
}

fn json_ok(body: Value) -> RouteResult {
    RouteResult::Json { status: 200, body }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpRequest;

    struct MockHub;

    impl ControlHub for MockHub {
        fn call(&self, method: Method, params: Value) -> Result<Value, GatewayError> {
            Ok(json!({"ok": true, "method": method.as_str(), "params": params}))
        }
    }

    fn req(method: &str, path: &str, body: &[u8]) -> HttpRequest {
        HttpRequest {
            method: method.into(),
            path: path.into(),
            headers: vec![],
            body: body.to_vec(),
        }
    }

    #[test]
    fn readonly_cannot_stop() {
        let err = dispatch(
            &req("POST", "/v1/vms/win/stop", b"{}"),
            Scope::ReadOnly,
            &MockHub,
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
        )
        .expect_err("forbidden");
        assert!(err.to_string().contains("forbidden"));
    }

    #[test]
    fn extra_json_field_rejected() {
        let err = dispatch(
            &req("POST", "/v1/vms/win/stop", br#"{"force":true,"nope":1}"#),
            Scope::Operator,
            &MockHub,
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
        )
        .expect_err("schema");
        assert!(err.to_string().contains("unknown field"));
    }
}
