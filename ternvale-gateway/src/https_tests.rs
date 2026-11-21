//! In-process HTTPS tests (no hypervisor).

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use ternvale_api::Method;

use crate::auth::{Scope, TokenStore};
use crate::config::{GatewayConfig, GatewayFile};
use crate::error::GatewayError;
use crate::hub::ControlHub;
use crate::serve::Gateway;
use crate::GatewayClient;

struct MockHub;

impl ControlHub for MockHub {
    fn call(
        &self,
        method: Method,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, GatewayError> {
        Ok(json!({"ok": true, "method": method.as_str(), "params": params}))
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tvgw-{}-{}-{tag}",
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

#[test]
fn https_list_and_forbidden_stop() {
    let dir = temp_dir("https");
    let config = GatewayConfig::from_file(&GatewayFile {
        listen: Some("127.0.0.1:0".into()),
        allow_remote_bind: false,
        require_mtls: false,
        max_body_bytes: None,
        rate_limit_per_minute: Some(30),
        state_dir: Some(dir.clone()),
        hub_socket: Some(dir.join("missing.sock")),
        run_dir: Some(dir.clone()),
        vms_dir: Some(dir.clone()),
    })
    .expect("config");
    crate::certs::generate_pki(&dir).expect("pki");
    let mut store = TokenStore::load(dir.join("tokens.tvtk")).expect("tokens");
    let (_id, ro) = store.issue(Scope::ReadOnly, None).expect("ro");
    let (_id, op) = store.issue(Scope::Operator, None).expect("op");
    let gw = Gateway::bind(config, Arc::new(MockHub)).expect("bind");
    std::thread::sleep(Duration::from_millis(80));
    let base = format!("https://127.0.0.1:{}", gw.addr().port());
    let ca = dir.join("ca.pem");
    let reader = GatewayClient::connect(&base, Some(ro), &ca, None, None).expect("client");
    let list = reader.json("GET", "/v1/vms", None).expect("list");
    assert_eq!(list["ok"], json!(true));
    let stop = reader
        .json("POST", "/v1/vms/win/stop", Some(&json!({})))
        .expect("stop json");
    // Client returns body even on 403; check error field.
    assert_eq!(stop["ok"], json!(false));
    let operator = GatewayClient::connect(&base, Some(op), &ca, None, None).expect("op client");
    let ok = operator
        .json("POST", "/v1/vms/win/stop", Some(&json!({})))
        .expect("stop");
    assert_eq!(ok["ok"], json!(true));
    let shot = operator
        .request("GET", "/v1/vms/win/screenshot.png", "application/json", &[])
        .expect("png");
    assert_eq!(shot.status, 200);
    assert!(crate::png::is_png(&shot.body));
    drop(gw);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn expired_token_is_unauthorized() {
    let dir = temp_dir("exp");
    let config = GatewayConfig::from_file(&GatewayFile {
        listen: Some("127.0.0.1:0".into()),
        allow_remote_bind: false,
        require_mtls: false,
        max_body_bytes: None,
        rate_limit_per_minute: Some(30),
        state_dir: Some(dir.clone()),
        hub_socket: Some(dir.join("missing.sock")),
        run_dir: Some(dir.clone()),
        vms_dir: Some(dir.clone()),
    })
    .expect("config");
    crate::certs::generate_pki(&dir).expect("pki");
    let mut store = TokenStore::load(dir.join("tokens.tvtk")).expect("tokens");
    let (_id, secret) = store.issue(Scope::Admin, Some(1)).expect("tok");
    let gw = Gateway::bind(config, Arc::new(MockHub)).expect("bind");
    std::thread::sleep(Duration::from_millis(80));
    let base = format!("https://127.0.0.1:{}", gw.addr().port());
    let client =
        GatewayClient::connect(&base, Some(secret), &dir.join("ca.pem"), None, None).expect("c");
    let body = client.json("GET", "/v1/vms", None).expect("json");
    assert_eq!(body["ok"], json!(false));
    assert!(body["error"].as_str().unwrap_or("").contains("expired"));
    drop(gw);
    let _ = std::fs::remove_dir_all(&dir);
}
