//! TLS accept loop and per-connection HTTP handling.

use std::io::{BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use rustls::{ServerConfig, ServerConnection, StreamOwned};

use crate::audit::{now_rfc3339, AuditEvent, AuditLog};
use crate::auth::{now_unix, AuthFail, Identity, Scope, TokenStore};
use crate::certs::PkiPaths;
use crate::config::GatewayConfig;
use crate::error::GatewayError;
use crate::http::{read_request, write_json_error, write_response, HttpRequest};
use crate::hub::{ControlHub, UnixHub};
use crate::rate::RateLimiter;
use crate::routes::{dispatch, RouteResult, WsKind};
use crate::tls::server_config;
use crate::ws;

/// Running gateway (listener thread).
pub struct Gateway {
    addr: std::net::SocketAddr,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Gateway {
    /// Bind `config`, generate missing PKI, serve until dropped.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
    pub fn bind(config: GatewayConfig, hub: Arc<dyn ControlHub>) -> Result<Self, GatewayError> {
        let paths = PkiPaths::in_dir(&config.state_dir);
        if !paths.ca_cert.is_file() {
            crate::certs::generate_pki(&config.state_dir)?;
        }
        let tls = server_config(&paths, config.require_mtls)?;
        let tokens_path = config.state_dir.join("tokens.tvtk");
        let _ = TokenStore::load(&tokens_path)?;
        let audit = AuditLog::open(config.state_dir.join("audit.jsonl"))?;
        let limiter = RateLimiter::new(config.rate_limit_per_minute);
        let listener = TcpListener::bind(config.listen)
            .map_err(|source| GatewayError::io_path("bind https", config.listen, source))?;
        listener
            .set_nonblocking(true)
            .map_err(|source| GatewayError::io("https nonblocking", source))?;
        let addr = listener
            .local_addr()
            .map_err(|source| GatewayError::io("local_addr", source))?;
        tracing::info!(
            target: "ternvale::gateway",
            listen = %addr,
            "https gateway listening"
        );
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let run_dir = config.run_dir.clone();
        let vms_dir = config.vms_dir.clone();
        let state = Arc::new(State {
            config,
            hub,
            tokens_path,
            audit,
            limiter,
            tls,
            run_dir,
            vms_dir,
        });
        let thread = thread::Builder::new()
            .name("gateway".into())
            .spawn(move || accept_loop(listener, flag, state))
            .map_err(|source| GatewayError::io("spawn gateway", source))?;
        Ok(Self {
            addr,
            stop,
            thread: Some(thread),
        })
    }

    /// Bound address (useful when listen port was 0).
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
    pub fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::error!(target: "ternvale::gateway", "gateway thread panicked");
            }
        }
    }
}

struct State {
    config: GatewayConfig,
    hub: Arc<dyn ControlHub>,
    tokens_path: PathBuf,
    audit: AuditLog,
    limiter: RateLimiter,
    tls: Arc<ServerConfig>,
    run_dir: PathBuf,
    vms_dir: PathBuf,
}

fn accept_loop(listener: TcpListener, stop: Arc<std::sync::atomic::AtomicBool>, state: Arc<State>) {
    while !stop.load(std::sync::atomic::Ordering::Acquire) {
        match listener.accept() {
            Ok((tcp, peer)) => {
                tracing::debug!(target: "ternvale::gateway", peer = %peer, "accept");
                let state = Arc::clone(&state);
                let _ = thread::Builder::new()
                    .name("gw-conn".into())
                    .spawn(move || {
                        if let Err(error) = handle_tcp(tcp, &state) {
                            tracing::warn!(
                                target: "ternvale::gateway",
                                error = %error,
                                "connection failed"
                            );
                        }
                    });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(error) => {
                tracing::error!(target: "ternvale::gateway", error = %error, "accept");
                thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }
}

fn handle_tcp(tcp: TcpStream, state: &State) -> Result<(), GatewayError> {
    tcp.set_nonblocking(false)
        .map_err(|source| GatewayError::io("tcp blocking", source))?;
    tcp.set_nodelay(true)
        .map_err(|source| GatewayError::io("tcp nodelay", source))?;
    let conn = ServerConnection::new(Arc::clone(&state.tls))
        .map_err(|error| GatewayError::Tls(error.to_string()))?;
    let mut tls = StreamOwned::new(conn, tcp);
    let mut reader = BufReader::new(&mut tls);
    let request = match read_request(&mut reader, state.config.max_body_bytes) {
        Ok(request) => request,
        Err(error) => {
            let msg = error.to_string();
            let lower = msg.to_ascii_lowercase();
            let tls_fail = ["certificate", "handshake", "peer", "alert", "tls"]
                .iter()
                .any(|n| lower.contains(n));
            let (status, reason, audit_reason) = if msg.contains("exceeds limit") {
                (413, "Payload Too Large", msg.as_str())
            } else if tls_fail {
                (401, "Unauthorized", "tls handshake failed")
            } else {
                (400, "Bad Request", msg.as_str())
            };
            audit(
                state,
                "unknown",
                "-",
                &HttpRequest {
                    method: "-".into(),
                    path: "-".into(),
                    headers: Vec::new(),
                    body: Vec::new(),
                },
                status,
                Some(audit_reason),
            );
            drop(reader);
            let _ = write_json_error(&mut tls, status, reason, audit_reason);
            return Ok(());
        }
    };
    drop(reader);
    let had_cert = tls
        .conn
        .peer_certificates()
        .map(|certs| !certs.is_empty())
        .unwrap_or(false);
    let result = handle_http(&mut tls, state, request, had_cert);
    tls.conn.send_close_notify();
    let _ = tls.flush();
    result
}

fn handle_http<W: Write + std::io::Read>(
    tls: &mut W,
    state: &State,
    request: HttpRequest,
    had_cert: bool,
) -> Result<(), GatewayError> {
    let ident = match authenticate(state, request.header("authorization"), had_cert) {
        Ok(ident) => ident,
        Err((status, reason, message)) => {
            audit(state, "anonymous", "-", &request, status, Some(message));
            write_json_error(tls, status, reason, message)?;
            return Ok(());
        }
    };
    if !state.limiter.allow(&ident.id) {
        audit(
            state,
            &ident.id,
            ident.scope.as_str(),
            &request,
            429,
            Some("rate limited"),
        );
        write_json_error(tls, 429, "Too Many Requests", "rate limited")?;
        return Ok(());
    }
    match dispatch(
        &request,
        ident.scope,
        state.hub.as_ref(),
        &state.run_dir,
        &state.vms_dir,
    ) {
        Ok(RouteResult::Json { status, body }) => {
            audit(
                state,
                &ident.id,
                ident.scope.as_str(),
                &request,
                status,
                None,
            );
            let bytes = serde_json::to_vec(&body)?;
            write_response(
                tls,
                status,
                "OK",
                "application/json; charset=utf-8",
                &bytes,
                &[],
            )
        }
        Ok(RouteResult::Bytes {
            status,
            content_type,
            body,
            extra,
        }) => {
            audit(
                state,
                &ident.id,
                ident.scope.as_str(),
                &request,
                status,
                None,
            );
            let extra: Vec<(&str, &str)> = extra
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            write_response(tls, status, "OK", content_type, &body, &extra)
        }
        Ok(RouteResult::WebSocket { kind, name }) => {
            if !ws::is_upgrade(&request) {
                write_json_error(tls, 426, "Upgrade Required", "websocket upgrade required")?;
                return Ok(());
            }
            audit(state, &ident.id, ident.scope.as_str(), &request, 101, None);
            ws::write_handshake(tls, &request)?;
            match kind {
                WsKind::Events => proxy_events(tls, &state.config.hub_socket),
                WsKind::Serial => {
                    let Some(name) = name else {
                        return Ok(());
                    };
                    serial_ws(tls, &state.vms_dir, &name)
                }
            }
        }
        Err(error) => {
            let (status, reason) = status_for(&error);
            audit(
                state,
                &ident.id,
                ident.scope.as_str(),
                &request,
                status,
                Some(&error.to_string()),
            );
            write_json_error(tls, status, reason, &error.to_string())
        }
    }
}

fn authenticate(
    state: &State,
    authorization: Option<&str>,
    had_cert: bool,
) -> Result<Identity, (u16, &'static str, &'static str)> {
    if authorization.is_some() {
        let store = TokenStore::load(&state.tokens_path)
            .map_err(|_| (500, "Internal Server Error", "token store unreadable"))?;
        return match store.authenticate(authorization, now_unix()) {
            Ok(ident) => Ok(ident),
            Err(AuthFail::Expired) => Err((401, "Unauthorized", "expired bearer token")),
            Err(AuthFail::Missing) => Err((401, "Unauthorized", "missing bearer token")),
            Err(_) => Err((401, "Unauthorized", "invalid bearer token")),
        };
    }
    if had_cert {
        return Ok(Identity {
            id: "mtls".into(),
            scope: Scope::Operator,
            via: "mtls",
        });
    }
    Err((401, "Unauthorized", "missing bearer token"))
}

fn audit(
    state: &State,
    who: &str,
    scope: &str,
    request: &HttpRequest,
    status: u16,
    reason: Option<&str>,
) {
    let event = AuditEvent {
        ts: now_rfc3339(),
        who: who.to_string(),
        scope: scope.to_string(),
        method: request.method.clone(),
        path: request.path.clone(),
        status,
        reason: reason.map(str::to_string),
    };
    if let Err(error) = state.audit.append(&event) {
        tracing::error!(target: "ternvale::gateway", error = %error, "audit append failed");
    }
}

fn status_for(error: &GatewayError) -> (u16, &'static str) {
    let text = error.to_string();
    if text.contains("forbidden") {
        (403, "Forbidden")
    } else if text.contains("not found") {
        (404, "Not Found")
    } else if text.contains("exceeds limit") {
        (413, "Payload Too Large")
    } else if text.starts_with("hub:") {
        (502, "Bad Gateway")
    } else {
        (400, "Bad Request")
    }
}

fn proxy_events<W: Write + std::io::Read>(tls: &mut W, hub: &Path) -> Result<(), GatewayError> {
    let mut stream = tls;
    match crate::hub::subscribe_unix(hub, |line| match ws::write_text(&mut stream, line) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }) {
        Ok(()) => {}
        Err(error) => {
            let payload = serde_json::json!({"ok": false, "error": error.to_string()});
            let _ = ws::write_text(stream, &payload.to_string());
        }
    }
    let _ = ws::write_close(stream);
    Ok(())
}

fn serial_ws<W: Write + std::io::Read>(
    tls: &mut W,
    vms_dir: &Path,
    name: &str,
) -> Result<(), GatewayError> {
    let Some(path) = crate::extras::serial_log_path(vms_dir, name) else {
        let _ = ws::write_text(tls, r#"{"ok":false,"error":"serial log not found"}"#);
        let _ = ws::write_close(tls);
        return Ok(());
    };
    crate::extras::follow_file(&path, |chunk| {
        let text = String::from_utf8_lossy(chunk);
        match ws::write_text(tls, &text) {
            Ok(()) => Ok(true),
            Err(_) => Ok(false),
        }
    })?;
    Ok(())
}

/// Build a [`UnixHub`] from config (CLI / ternvaled).
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn unix_hub(config: &GatewayConfig) -> Arc<dyn ControlHub> {
    Arc::new(UnixHub::new(&config.hub_socket))
}
