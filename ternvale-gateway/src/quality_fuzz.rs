//! Malformed JSON against REST endpoints. Wall-clock duration is
//! `TERNVALE_GATEWAY_FUZZ_SECS` (default 3; 600 is a 10-minute live analog).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use ternvale_api::Method;

use crate::auth::{Scope, TokenStore};
use crate::config::{GatewayConfig, GatewayFile};
use crate::error::GatewayError;
use crate::hub::ControlHub;
use crate::serve::Gateway;
use crate::GatewayClient;

struct OkHub;

impl ControlHub for OkHub {
    fn call(
        &self,
        method: Method,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, GatewayError> {
        Ok(json!({"ok": true, "method": method.as_str(), "params": params}))
    }
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tvgw-fuzz-{}-{}",
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

/// Junk bodies for POST `/v1/vms/win/stop` (schema + HTTP parser).
fn payloads(i: u32) -> &'static [u8] {
    match i % 12 {
        0 => b"{",
        1 => b"[]",
        2 => b"null",
        3 => b"{\"force\":\"yes\"}",
        4 => b"{\"name\":\"win\",\"nope\":1}",
        5 => b"",
        6 => b"{\"force\":true,\"extra\":[]}",
        7 => b"\xff\xfe{\"force\":true}",
        8 => b"{\"timeout_ms\":\"x\"}",
        9 => b"{\"force\":true,\"graceful\":true,\"timeout_ms\":1}",
        10 => b"\"string\"",
        _ => b"{\"force\":true}",
    }
}

#[test]
fn fuzz_json_endpoints_no_panic() {
    let secs: u64 = std::env::var("TERNVALE_GATEWAY_FUZZ_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let dir = scratch();
    crate::certs::generate_pki(&dir).expect("pki");
    let mut store = TokenStore::load(dir.join("tokens.tvtk")).expect("tokens");
    let (_id, op) = store.issue(Scope::Operator, None).expect("op");
    let gw = Gateway::bind(
        GatewayConfig::from_file(&GatewayFile {
            listen: Some("127.0.0.1:0".into()),
            allow_remote_bind: false,
            require_mtls: false,
            max_body_bytes: None,
            rate_limit_per_minute: Some(10_000),
            state_dir: Some(dir.clone()),
            hub_socket: Some(dir.join("missing.sock")),
            run_dir: Some(dir.clone()),
            vms_dir: Some(dir.clone()),
        })
        .expect("cfg"),
        Arc::new(OkHub),
    )
    .expect("bind");
    std::thread::sleep(Duration::from_millis(80));
    let base = format!("https://127.0.0.1:{}", gw.addr().port());
    let curl = GatewayClient::connect(&base, Some(op), &dir.join("ca.pem"), None, None).expect("c");
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut i = 0u32;
    let panics = Mutex::new(0u32);
    tracing::info!(
        target: "ternvale::gateway",
        secs,
        "quality json fuzz start"
    );
    while Instant::now() < deadline {
        let body = payloads(i);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = curl.request("POST", "/v1/vms/win/stop", "application/json", body);
            let _ = curl.request("POST", "/v1/vms", "application/json", body);
            let _ = curl.request("GET", "/v1/vms", "application/json", &[]);
        }));
        if result.is_err() {
            *panics.lock().expect("p") += 1;
        }
        i = i.wrapping_add(1);
    }
    let health = curl.json("GET", "/v1/vms", None).expect("still serving");
    assert_eq!(health["ok"], json!(true));
    assert_eq!(*panics.lock().expect("p"), 0);
    tracing::info!(
        target: "ternvale::gateway",
        rounds = i,
        secs,
        "quality json fuzz finished without panics"
    );
    drop(gw);
    let _ = std::fs::remove_dir_all(&dir);
}
