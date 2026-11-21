//! JSON-RPC client of `api.sock`. The gateway does not duplicate VM lifecycle.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use ternvale_api::{
    HandshakeParams, JsonRpcRequest, JsonRpcResponse, Method, API_VERSION, JSONRPC_VERSION,
};

use crate::error::GatewayError;

const MAX_LINE: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

/// Control plane used by REST routes.
pub trait ControlHub: Send + Sync {
    /// JSON-RPC call after handshake.
    fn call(&self, method: Method, params: Value) -> Result<Value, GatewayError>;
}

/// Unix-socket hub (`ternvale serve` / `ternvaled`).
#[derive(Debug, Clone)]
pub struct UnixHub {
    path: PathBuf,
}

impl UnixHub {
    /// Talk to `path`.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(path = %path.as_ref().display()))]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
}

impl ControlHub for UnixHub {
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(method = method.as_str()))]
    fn call(&self, method: Method, params: Value) -> Result<Value, GatewayError> {
        call_unix(&self.path, method, params)
    }
}

/// One handshake + method on `path`.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(path = %path.display(), method = method.as_str()))]
pub fn call_unix(path: &Path, method: Method, params: Value) -> Result<Value, GatewayError> {
    let stream = UnixStream::connect(path)
        .map_err(|source| GatewayError::io_path("connect hub", path.display(), source))?;
    stream
        .set_read_timeout(Some(TIMEOUT))
        .map_err(|source| GatewayError::io("hub read timeout", source))?;
    stream
        .set_write_timeout(Some(TIMEOUT))
        .map_err(|source| GatewayError::io("hub write timeout", source))?;
    let mut writer = stream
        .try_clone()
        .map_err(|source| GatewayError::io("hub clone", source))?;
    let mut reader = BufReader::new(stream);
    handshake(&mut writer, &mut reader)?;
    rpc(&mut writer, &mut reader, 2, method.as_str(), params)
}

/// Handshake, `subscribe`, then read event notifications until `emit` returns false.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(path = %path.display()))]
pub fn subscribe_unix(
    path: &Path,
    mut emit: impl FnMut(&str) -> Result<bool, GatewayError>,
) -> Result<(), GatewayError> {
    let stream = UnixStream::connect(path)
        .map_err(|source| GatewayError::io_path("connect hub subscribe", path.display(), source))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(300)))
        .map_err(|source| GatewayError::io("hub subscribe timeout", source))?;
    let mut writer = stream
        .try_clone()
        .map_err(|source| GatewayError::io("hub subscribe clone", source))?;
    let mut reader = BufReader::new(stream);
    handshake(&mut writer, &mut reader)?;
    let _ = rpc(
        &mut writer,
        &mut reader,
        2,
        Method::Subscribe.as_str(),
        serde_json::json!({
            "events": ["state", "agent-connect", "agent-disconnect", "guest-tools-outdated", "log", "install-progress"]
        }),
    )?;
    let mut line = String::new();
    loop {
        line.clear();
        let n = match reader.read_line(&mut line) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(error) => return Err(GatewayError::io("read hub event", error)),
        };
        if n == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !emit(trimmed)? {
            return Ok(());
        }
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn handshake<W: Write, R: BufRead>(writer: &mut W, reader: &mut R) -> Result<(), GatewayError> {
    let params = serde_json::to_value(HandshakeParams {
        api_version: API_VERSION,
    })?;
    let result = rpc(writer, reader, 1, Method::Handshake.as_str(), params)?;
    if result.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(GatewayError::Hub("handshake rejected".into()));
    }
    Ok(())
}

#[tracing::instrument(
    level = "debug",
    target = "ternvale::gateway",
    skip_all,
    fields(method)
)]
fn rpc<W: Write, R: BufRead>(
    writer: &mut W,
    reader: &mut R,
    id: i64,
    method: &str,
    params: Value,
) -> Result<Value, GatewayError> {
    let request = JsonRpcRequest {
        jsonrpc: JSONRPC_VERSION.to_string(),
        method: method.to_string(),
        params: Some(params),
        id: Some(serde_json::json!(id)),
    };
    let mut line = serde_json::to_string(&request)?;
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .map_err(|source| GatewayError::io("write hub request", source))?;
    writer
        .flush()
        .map_err(|source| GatewayError::io("flush hub request", source))?;
    let mut response_line = String::new();
    loop {
        response_line.clear();
        let n = reader
            .read_line(&mut response_line)
            .map_err(|source| GatewayError::io("read hub response", source))?;
        if n == 0 {
            return Err(GatewayError::Hub("hub closed".into()));
        }
        if response_line.len() > MAX_LINE {
            return Err(GatewayError::Hub("hub line too long".into()));
        }
        let trimmed = response_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Notifications (events) have no id; skip them on this short-lived call.
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if value.get("id").is_none() && value.get("method").is_some() {
                continue;
            }
        }
        let response: JsonRpcResponse = serde_json::from_str(trimmed)?;
        if let Some(error) = response.error {
            return Err(GatewayError::Hub(error.message));
        }
        return response
            .result
            .ok_or_else(|| GatewayError::Hub("hub response missing result".into()));
    }
}
