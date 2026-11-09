//! Supervisor JSON-RPC socket (`api.sock`): list/create/start and forwards.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use ternvale_api::{
    is_jsonrpc_line, parse_request, ApiError, Event, EventKind, HandshakeParams, HandshakeResult,
    JsonRpcResponse, Method, JSONRPC_VERSION,
};

use crate::paths;
use crate::protocol::Response;

const ACCEPT_POLL: Duration = Duration::from_millis(50);
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_LINE: usize = 64 * 1024;

/// Hub listener. Dropping it stops the thread and removes the socket.
pub struct ApiServer {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ApiServer {
    /// Bind [`paths::api_socket`].
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
    pub fn start() -> Result<Self> {
        Self::bind(&paths::api_socket()?)
    }

    /// Bind `path` (tests).
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(path = %path.display()))]
    pub fn bind(path: &Path) -> Result<Self> {
        let dir = path
            .parent()
            .with_context(|| format!("api socket {} has no parent", path.display()))?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create run directory {}", dir.display()))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restrict run directory {}", dir.display()))?;
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
        let listener = UnixListener::bind(path)
            .with_context(|| format!("bind api socket {}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restrict api socket {}", path.display()))?;
        listener
            .set_nonblocking(true)
            .with_context(|| format!("make api socket {} non-blocking", path.display()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("api".to_string())
            .spawn(move || accept_loop(&listener, &flag))
            .context("spawn api socket thread")?;
        tracing::info!(target: "ternvale::api", path = %path.display(), "api socket listening");
        Ok(Self {
            path: path.to_path_buf(),
            stop,
            thread: Some(thread),
        })
    }

    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ApiServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::error!(target: "ternvale::api", "api socket thread panicked");
            }
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn accept_loop(listener: &UnixListener, stop: &AtomicBool) {
    let mut next_id = 0u64;
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                next_id += 1;
                let id = next_id;
                let spawned = std::thread::Builder::new()
                    .name(format!("api-{id}"))
                    .spawn(move || serve(stream, id));
                if let Err(error) = spawned {
                    tracing::error!(target: "ternvale::api", conn = id, error = %error, "spawn api conn");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(error) => {
                tracing::warn!(target: "ternvale::api", error = %error, "api accept failed");
                std::thread::sleep(ACCEPT_POLL);
            }
        }
    }
}

fn serve(stream: UnixStream, id: u64) {
    let span = tracing::info_span!(target: "ternvale::api", "hub", conn = id);
    let _entered = span.enter();
    if let Err(error) = crate::peer::authorize(&stream) {
        tracing::warn!(target: "ternvale::api", error = %format!("{error:#}"), "hub peer rejected");
        return;
    }
    if let Err(error) = serve_lines(stream) {
        tracing::warn!(target: "ternvale::api", error = %format!("{error:#}"), "hub connection error");
    }
}

fn serve_lines(stream: UnixStream) -> Result<()> {
    stream.set_nonblocking(false).context("hub blocking")?;
    stream
        .set_read_timeout(Some(IDLE_TIMEOUT))
        .context("hub timeout")?;
    let mut writer = stream.try_clone().context("clone hub conn")?;
    let mut reader = BufReader::new(stream);
    let mut handshook = false;
    loop {
        let mut line = String::new();
        let n = reader
            .by_ref()
            .take(MAX_LINE as u64 + 1)
            .read_line(&mut line)
            .context("read hub request")?;
        if n == 0 {
            return Ok(());
        }
        if line.len() > MAX_LINE {
            return Ok(());
        }
        match dispatch_hub(line.trim_end(), &mut handshook) {
            HubDispatch::Reply(out) => {
                writer.write_all(out.as_bytes())?;
                writer.write_all(b"\n")?;
            }
            HubDispatch::Subscribe { reply, kinds } => {
                writer.write_all(reply.as_bytes())?;
                writer.write_all(b"\n")?;
                stream_hub_events(&mut writer, &kinds);
                return Ok(());
            }
        }
    }
}

enum HubDispatch {
    Reply(String),
    Subscribe {
        reply: String,
        kinds: Vec<EventKind>,
    },
}

#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
fn dispatch_hub(line: &str, handshook: &mut bool) -> HubDispatch {
    if !is_jsonrpc_line(line) {
        let err = ApiError::InvalidRequest("hub requires JSON-RPC 2.0".into());
        return rpc_err(Value::Null, &err);
    }
    let request = match parse_request(line) {
        Ok(r) => r,
        Err(error) => return rpc_err(Value::Null, &error),
    };
    let id = request.id.clone().unwrap_or(Value::Null);
    let Some(method) = Method::parse(&request.method) else {
        return rpc_err(id, &ApiError::MethodNotFound(request.method.clone()));
    };
    if method != Method::Handshake && !*handshook {
        return rpc_err(id, &ApiError::InvalidRequest("handshake required".into()));
    }
    tracing::info!(target: "ternvale::api", method = method.as_str(), "hub request");
    if method == Method::Handshake {
        return match handshake(request.params.as_ref()) {
            Ok(result) => {
                *handshook = true;
                rpc_ok(id, serde_json::to_value(result).unwrap_or(Value::Null))
            }
            Err(error) => rpc_err(id, &error),
        };
    }
    if method == Method::Subscribe {
        let kinds = ternvale_api::EVENT_KINDS.to_vec();
        let names: Vec<String> = kinds.iter().map(|k| k.as_str().to_string()).collect();
        let reply = JsonRpcResponse::result(
            id,
            serde_json::to_value(Response::subscribed(names)).unwrap_or(Value::Null),
        );
        return HubDispatch::Subscribe {
            reply: encode(&reply),
            kinds,
        };
    }
    match crate::api_hub::hub_call(method, request.params.as_ref()) {
        Ok(response) => rpc_ok(id, serde_json::to_value(&response).unwrap_or(Value::Null)),
        Err(error) => rpc_err(id, &error),
    }
}

fn handshake(params: Option<&Value>) -> Result<HandshakeResult, ApiError> {
    let params: HandshakeParams = serde_json::from_value(params.cloned().unwrap_or(Value::Null))
        .map_err(|e| ApiError::InvalidParams(e.to_string()))?;
    HandshakeResult::negotiate(params.api_version)
}

fn stream_hub_events(writer: &mut UnixStream, kinds: &[EventKind]) {
    let mut last: Vec<(String, bool)> = Vec::new();
    loop {
        let vms = match crate::api_library::list_vms() {
            Ok(vms) => vms,
            Err(error) => {
                tracing::warn!(target: "ternvale::api", error = %format!("{error:#}"), "hub list for events");
                Vec::new()
            }
        };
        if kinds.contains(&EventKind::State) {
            for vm in &vms {
                let prev = last.iter().find(|(n, _)| n == &vm.name).map(|(_, r)| *r);
                if prev != Some(vm.running) {
                    let event = Event {
                        kind: EventKind::State,
                        vm: vm.name.clone(),
                        state: Some(if vm.running {
                            "running".into()
                        } else {
                            "stopped".into()
                        }),
                        tools_outdated: None,
                        line: None,
                    };
                    if !write_event(writer, &event) {
                        return;
                    }
                }
            }
            last = vms.into_iter().map(|v| (v.name, v.running)).collect();
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn write_event(writer: &mut UnixStream, event: &Event) -> bool {
    let mut line = match serde_json::to_string(&event.notification()) {
        Ok(s) => s,
        Err(_) => return false,
    };
    line.push('\n');
    writer.write_all(line.as_bytes()).is_ok()
}

fn rpc_ok(id: Value, result: Value) -> HubDispatch {
    HubDispatch::Reply(encode(&JsonRpcResponse::result(id, result)))
}

fn rpc_err(id: Value, error: &ApiError) -> HubDispatch {
    HubDispatch::Reply(encode(&JsonRpcResponse::from_api_error(id, error)))
}

fn encode(value: &JsonRpcResponse) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| {
        format!(
            r#"{{"jsonrpc":"{JSONRPC_VERSION}","error":{{"code":-32603,"message":"encode failed"}},"id":null}}"#
        )
    })
}
