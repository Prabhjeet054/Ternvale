//! JSON-RPC method dispatch for the hub (`api.sock`).

use std::path::Path;

use serde_json::Value;
use ternvale_api::{ApiError, Method};

use crate::protocol::{Request, Response};

/// Run one hub method after handshake.
#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(method = method.as_str()))]
pub fn hub_call(method: Method, params: Option<&Value>) -> Result<Response, ApiError> {
    let map = match params {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => return Err(ApiError::InvalidParams("params must be an object".into())),
    };
    let name = || {
        map.get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ApiError::InvalidParams("name required".into()))
    };
    let lib = |r: anyhow::Result<()>| match r {
        Ok(()) => match crate::api_library::list_vms() {
            Ok(vms) => Ok(Response::vms(vms)),
            Err(error) => Ok(Response::error(format!("{error:#}"))),
        },
        Err(error) => Ok(Response::error(format!("{error:#}"))),
    };
    match method {
        Method::List => crate::api_library::list_vms()
            .map(Response::vms)
            .map_err(|e| ApiError::Application(format!("{e:#}"))),
        Method::Create => {
            let n = name()?;
            lib(crate::api_library::create_vm(
                &n,
                map.get("config_toml").and_then(Value::as_str),
                map.get("config_path")
                    .and_then(Value::as_str)
                    .map(Path::new),
            ))
        }
        Method::Delete => lib(crate::api_library::delete_stopped_vm(&name()?)),
        Method::Start => lib(crate::api_library::start_vm(&name()?)),
        Method::Clone => {
            let source = map
                .get("source")
                .and_then(Value::as_str)
                .ok_or_else(|| ApiError::InvalidParams("source".into()))?;
            let new_name = map
                .get("new_name")
                .and_then(Value::as_str)
                .ok_or_else(|| ApiError::InvalidParams("new_name".into()))?;
            lib(crate::clone_cmd::clone_vm(source, new_name).map(|_| ()))
        }
        Method::Stop
        | Method::Pause
        | Method::Resume
        | Method::Stats
        | Method::Balloon
        | Method::SnapshotCreate
        | Method::SnapshotRestore
        | Method::SnapshotList
        | Method::SnapshotDelete
        | Method::Status => {
            let n = name()?;
            let req = vm_request(method, &map)?;
            crate::api_library::forward(&n, &req)
                .map_err(|e| ApiError::Application(format!("{e:#}")))
        }
        Method::MediaAttach => lib(crate::api_library::media_attach(
            &name()?,
            map.get("slot").and_then(Value::as_str).unwrap_or(""),
            Path::new(map.get("path").and_then(Value::as_str).unwrap_or("")),
        )),
        Method::MediaDetach => lib(crate::api_library::media_detach(
            &name()?,
            map.get("slot").and_then(Value::as_str).unwrap_or(""),
        )),
        Method::UsbAttach => {
            let vendor = map
                .get("vendor_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| ApiError::InvalidParams("vendor_id".into()))?
                as u16;
            let product = map
                .get("product_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| ApiError::InvalidParams("product_id".into()))?
                as u16;
            let port = map.get("port").and_then(Value::as_u64).map(|p| p as u8);
            lib(crate::api_library::usb_attach(
                &name()?,
                vendor,
                product,
                port,
            ))
        }
        Method::NetConfigure => {
            let index = map
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| ApiError::InvalidParams("index".into()))?
                as usize;
            lib(crate::api_library::net_configure(
                &name()?,
                index,
                map.get("backend")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                map.get("mode").and_then(Value::as_str).map(str::to_string),
                map.get("network")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                map.get("bridge_interface")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ))
        }
        Method::Handshake | Method::Subscribe => Err(ApiError::InvalidRequest("internal".into())),
    }
}

fn vm_request(method: Method, map: &serde_json::Map<String, Value>) -> Result<Request, ApiError> {
    let timeout = map.get("timeout_ms").and_then(Value::as_u64);
    let snap = || {
        map.get("snapshot")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ApiError::InvalidParams("snapshot".into()))
    };
    Ok(match method {
        Method::Status => Request::Status,
        Method::Pause => Request::Pause {
            timeout_ms: timeout,
        },
        Method::Resume => Request::Resume,
        Method::Stop => {
            if map.get("force").and_then(Value::as_bool) == Some(true) {
                Request::ForceStop
            } else if map.get("graceful").and_then(Value::as_bool) == Some(true) {
                Request::GracefulStop {
                    timeout_ms: timeout,
                }
            } else {
                Request::Shutdown
            }
        }
        Method::Stats => Request::QueryStats,
        Method::Balloon => Request::BalloonSet {
            target_mib: map.get("target_mib").and_then(Value::as_u64),
            auto: map.get("auto").and_then(Value::as_bool),
        },
        Method::SnapshotCreate => Request::SnapshotCreate { name: snap()? },
        Method::SnapshotRestore => Request::SnapshotRestore { name: snap()? },
        Method::SnapshotList => Request::SnapshotList,
        Method::SnapshotDelete => Request::SnapshotDelete { name: snap()? },
        _ => return Err(ApiError::MethodNotFound(method.as_str().into())),
    })
}
