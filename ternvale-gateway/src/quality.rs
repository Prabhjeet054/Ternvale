//! Step 118 host analogs: curl-like HTTPS, tokens, bind scan, serial WS, PNG.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::{ClientConnection, StreamOwned};
use serde_json::json;
use ternvale_api::Method;

use crate::auth::{Scope, TokenSecret, TokenStore};
use crate::config::{GatewayConfig, GatewayFile};
use crate::error::GatewayError;
use crate::hub::ControlHub;
use crate::serve::Gateway;
use crate::GatewayClient;

struct RecHub {
    calls: Mutex<Vec<&'static str>>,
}

impl ControlHub for RecHub {
    fn call(
        &self,
        method: Method,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, GatewayError> {
        self.calls.lock().expect("calls").push(method.as_str());
        tracing::info!(
            target: "ternvale::gateway",
            method = method.as_str(),
            "quality hub analog"
        );
        Ok(json!({"ok": true, "method": method.as_str(), "params": params}))
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tvgw-q-{}-{}-{tag}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

fn file(dir: &Path, rate: u32, mtls: bool) -> GatewayFile {
    GatewayFile {
        listen: Some("127.0.0.1:0".into()),
        allow_remote_bind: false,
        require_mtls: mtls,
        max_body_bytes: None,
        rate_limit_per_minute: Some(rate),
        state_dir: Some(dir.to_path_buf()),
        hub_socket: Some(dir.join("missing.sock")),
        run_dir: Some(dir.to_path_buf()),
        vms_dir: Some(dir.to_path_buf()),
    }
}

fn boot(
    dir: &Path,
    rate: u32,
    mtls: bool,
) -> (Gateway, String, TokenSecret, TokenSecret, TokenSecret) {
    crate::certs::generate_pki(dir).expect("pki");
    let mut store = TokenStore::load(dir.join("tokens.tvtk")).expect("tokens");
    let (_a, admin) = store.issue(Scope::Admin, None).expect("admin");
    let (_r, ro) = store.issue(Scope::ReadOnly, None).expect("ro");
    let (_o, op) = store.issue(Scope::Operator, None).expect("op");
    let gw = Gateway::bind(
        GatewayConfig::from_file(&file(dir, rate, mtls)).expect("cfg"),
        Arc::new(RecHub {
            calls: Mutex::new(Vec::new()),
        }),
    )
    .expect("bind");
    std::thread::sleep(Duration::from_millis(80));
    let base = format!("https://127.0.0.1:{}", gw.addr().port());
    tracing::info!(
        target: "ternvale::gateway",
        listen = %gw.addr(),
        "quality daemon analog listening"
    );
    (gw, base, admin, ro, op)
}

fn client(base: &str, token: TokenSecret, ca: &Path) -> GatewayClient {
    GatewayClient::connect(base, Some(token), ca, None, None).expect("client")
}

#[test]
fn curl_create_start_snapshot_and_readonly_cannot_stop() {
    let dir = scratch("curl");
    let (gw, base, admin, ro, _op) = boot(&dir, 60, false);
    let ca = dir.join("ca.pem");
    let curl = client(&base, admin, &ca);
    let created = curl
        .json(
            "POST",
            "/v1/vms",
            Some(&json!({"name": "win", "config_toml": "name = \"win\"\n"})),
        )
        .expect("create");
    assert_eq!(created["ok"], json!(true));
    assert_eq!(created["method"], json!("create"));
    let started = curl
        .json("POST", "/v1/vms/win/start", Some(&json!({})))
        .expect("start");
    assert_eq!(started["method"], json!("start"));
    let snap = curl
        .json(
            "POST",
            "/v1/vms/win/snapshots",
            Some(&json!({"snapshot": "snap1"})),
        )
        .expect("snap");
    assert_eq!(snap["method"], json!("snapshot.create"));
    let reader = client(&base, ro, &ca);
    let list = reader.json("GET", "/v1/vms", None).expect("list");
    assert_eq!(list["ok"], json!(true));
    let stop = reader
        .request("POST", "/v1/vms/win/stop", "application/json", b"{}")
        .expect("stop");
    assert_eq!(stop.status, 403);
    drop(gw);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wrong_client_cert_and_expired_token_are_rejected_and_audited() {
    let dir = scratch("authz");
    let (gw, base, _admin, _ro, _op) = boot(&dir, 60, false);
    let ca = dir.join("ca.pem");
    let mut store = TokenStore::load(dir.join("tokens.tvtk")).expect("reload");
    let (_id, expired) = store.issue(Scope::Admin, Some(1)).expect("expired");
    let dead = client(&base, expired, &ca);
    let body = dead.json("GET", "/v1/vms", None).expect("expired json");
    assert!(body["error"].as_str().unwrap_or("").contains("expired"));
    let other = scratch("otherca");
    crate::certs::generate_pki(&other).expect("other pki");
    let bad = GatewayClient::connect(
        &base,
        None,
        &ca,
        Some(&other.join("client.pem")),
        Some(&other.join("client.key")),
    )
    .expect("bad client cfg");
    let err = bad
        .request("GET", "/v1/vms", "application/json", &[])
        .expect_err("wrong cert");
    tracing::info!(target: "ternvale::gateway", error = %err, "wrong client cert rejected");
    std::thread::sleep(Duration::from_millis(50));
    let audit = std::fs::read_to_string(dir.join("audit.jsonl")).expect("audit");
    assert!(audit.contains("expired bearer token"), "{audit}");
    assert!(
        audit.contains("tls handshake failed") || audit.contains("invalid bearer"),
        "{audit}"
    );
    drop(gw);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other);
}

#[test]
fn default_bind_is_loopback_lsof_analog() {
    let dir = scratch("bind");
    let (gw, base, admin, _ro, _op) = boot(&dir, 60, false);
    let addr = gw.addr();
    assert!(addr.ip().is_loopback(), "{addr}");
    let lsof = Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{}", addr.port()), "-sTCP:LISTEN"])
        .output();
    if let Ok(out) = lsof {
        let text = String::from_utf8_lossy(&out.stdout);
        tracing::info!(target: "ternvale::gateway", lsof = %text, "listen scan");
        if !text.is_empty() {
            assert!(text.contains("127.0.0.1"), "{text}");
            assert!(!text.contains(&format!("*:{}", addr.port())), "{text}");
        }
    }
    if Command::new("nmap").arg("-V").output().is_ok() {
        let scan = Command::new("nmap")
            .args(["-p", &addr.port().to_string(), "127.0.0.1"])
            .output()
            .expect("nmap localhost");
        let text = String::from_utf8_lossy(&scan.stdout);
        tracing::info!(target: "ternvale::gateway", nmap = %text, "nmap localhost analog");
    }
    let _ = client(&base, admin, &dir.join("ca.pem"))
        .json("GET", "/v1/vms", None)
        .expect("loopback serve");
    drop(gw);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hammer_triggers_rate_limit() {
    let dir = scratch("rate");
    let (gw, base, admin, _ro, _op) = boot(&dir, 3, false);
    let curl = client(&base, admin, &dir.join("ca.pem"));
    let mut last = 0;
    for _ in 0..6 {
        last = curl
            .request("GET", "/v1/vms", "application/json", &[])
            .expect("hit")
            .status;
    }
    assert_eq!(last, 429);
    let audit = std::fs::read_to_string(dir.join("audit.jsonl")).expect("audit");
    assert!(audit.contains("rate limited"), "{audit}");
    drop(gw);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn serial_websocket_and_valid_png_screenshot() {
    let dir = scratch("ws");
    let serial = dir.join("guest-serial.log");
    std::fs::write(&serial, "").expect("serial");
    std::fs::create_dir_all(dir.join("win")).expect("vm dir");
    std::fs::write(
        dir.join("win/config.toml"),
        format!("serial_log = \"{}\"\n", serial.display()),
    )
    .expect("config");
    let mut fb = Vec::from(*crate::extras::FB_MAGIC);
    fb.extend_from_slice(&crate::extras::FB_VERSION.to_le_bytes());
    fb.extend_from_slice(&2u32.to_le_bytes());
    fb.extend_from_slice(&1u32.to_le_bytes());
    fb.extend_from_slice(&[255, 0, 0, 255, 0, 255, 0, 255]);
    std::fs::write(dir.join("win.fb"), fb).expect("fb");
    let (gw, base, admin, _ro, _op) = boot(&dir, 60, false);
    let curl = client(&base, admin.clone(), &dir.join("ca.pem"));
    let shot = curl
        .request("GET", "/v1/vms/win/screenshot.png", "application/json", &[])
        .expect("png");
    assert_eq!(shot.status, 200);
    assert!(crate::png::is_png(&shot.body));
    let port = gw.addr().port();
    let frame = serial_frame(port, &dir.join("ca.pem"), &admin, &serial);
    assert!(frame.contains("hello-uart"), "{frame:?}");
    drop(gw);
    let _ = std::fs::remove_dir_all(&dir);
}

fn serial_frame(port: u16, ca: &Path, token: &TokenSecret, serial: &Path) -> String {
    crate::tls::install_ring();
    let tls_cfg = crate::tls::client_config(ca, None, None).expect("tls");
    let name = crate::tls::server_name("127.0.0.1").expect("sni");
    let conn = ClientConnection::new(tls_cfg, name).expect("conn");
    let tcp = std::net::TcpStream::connect(("127.0.0.1", port)).expect("tcp");
    tcp.set_read_timeout(Some(Duration::from_secs(3)))
        .expect("timeout");
    let mut stream = StreamOwned::new(conn, tcp);
    let req = format!(
        "GET /v1/vms/win/serial HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nAuthorization: Bearer {}\r\n\r\n",
        token.expose()
    );
    stream.write_all(req.as_bytes()).expect("ws req");
    stream.flush().expect("flush");
    let mut buf = vec![0u8; 2048];
    let n = stream.read(&mut buf).expect("ws 101");
    let head = String::from_utf8_lossy(&buf[..n]);
    assert!(head.contains("101"), "{head}");
    let path = serial.to_path_buf();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append serial");
        f.write_all(b"hello-uart\n").expect("write serial");
    });
    let payload = crate::ws::read_data_frame(&mut stream).expect("frame");
    String::from_utf8_lossy(&payload.unwrap_or_default()).into_owned()
}
