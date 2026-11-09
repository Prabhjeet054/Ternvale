//! JSON-RPC handshake, hub list/create, and subscribe on the VM socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use ternvale_vmm::VmControl;

use crate::api_server::ApiServer;
use crate::paths::{RUN_DIR_ENV, VMS_DIR_ENV};
use crate::protocol::Response;
use crate::server::ControlServer;
use crate::tests::CHILD_PROCESS;

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tv-api-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rpc(stream: &UnixStream, line: &str) -> serde_json::Value {
    let mut writer = stream.try_clone().expect("clone");
    writer.write_all(line.as_bytes()).expect("write");
    writer.write_all(b"\n").expect("nl");
    let mut reply = String::new();
    BufReader::new(stream.try_clone().expect("r"))
        .read_line(&mut reply)
        .expect("read");
    serde_json::from_str(reply.trim()).expect("json")
}

#[test]
fn jsonrpc_handshake_then_status_on_vm_socket() {
    let dir = Scratch::new("vm");
    let path = dir.0.join("vm.sock");
    let control = Arc::new(VmControl::new("vm", 1));
    let _server = ControlServer::start(&path, control).expect("start");
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "socket mode {mode:#o}");
    let stream = UnixStream::connect(&path).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("to");
    let denied = rpc(&stream, r#"{"jsonrpc":"2.0","method":"status","id":1}"#);
    assert_eq!(denied["error"]["code"], -32600);
    let hs = rpc(
        &stream,
        r#"{"jsonrpc":"2.0","method":"handshake","params":{"api_version":1},"id":2}"#,
    );
    assert_eq!(hs["result"]["api_version"], 1);
    assert_eq!(hs["result"]["ok"], true);
    let status = rpc(&stream, r#"{"jsonrpc":"2.0","method":"status","id":3}"#);
    assert_eq!(status["result"]["ok"], true);
    assert_eq!(status["result"]["status"]["name"], "vm");
    let legacy: Response = {
        let mut w = stream.try_clone().expect("c");
        w.write_all(b"{\"cmd\":\"status\"}\n").expect("w");
        let mut line = String::new();
        BufReader::new(stream.try_clone().expect("r"))
            .read_line(&mut line)
            .expect("r");
        serde_json::from_str(line.trim()).expect("legacy")
    };
    assert!(legacy.ok);
}

#[test]
fn hub_list_create_delete() {
    let _guard = CHILD_PROCESS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let scratch = Scratch::new("hub");
    let vms = scratch.0.join("vms");
    let run = scratch.0.join("run");
    std::fs::create_dir_all(&vms).expect("vms");
    std::fs::create_dir_all(&run).expect("run");
    let prev_vms = std::env::var_os(VMS_DIR_ENV);
    let prev_run = std::env::var_os(RUN_DIR_ENV);
    // SAFETY: this test owns the env keys; workspace tests use --test-threads=1.
    unsafe {
        std::env::set_var(VMS_DIR_ENV, &vms);
        std::env::set_var(RUN_DIR_ENV, &run);
    }
    let sock = run.join("api.sock");
    let _server = ApiServer::bind(&sock).expect("bind");
    let mode = std::fs::metadata(&sock).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let stream = UnixStream::connect(&sock).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("to");
    rpc(
        &stream,
        r#"{"jsonrpc":"2.0","method":"handshake","params":{"api_version":1},"id":1}"#,
    );
    let listed = rpc(&stream, r#"{"jsonrpc":"2.0","method":"list","id":2}"#);
    assert_eq!(listed["result"]["ok"], true);
    assert_eq!(listed["result"]["vms"], serde_json::json!([]));

    let kernel = scratch.0.join("Image");
    std::fs::write(&kernel, b"k").expect("kernel");
    let toml = format!(
        r#"
name = "lab"
cpus = 1
ram_mib = 256
kernel = "{k}"
serial_log = "{s}"
cmdline = "console=ttyAMA0"
"#,
        k = kernel.display(),
        s = scratch.0.join("serial.log").display(),
    );
    let create = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "create",
        "params": { "name": "lab", "config_toml": toml },
        "id": 3
    });
    let created = rpc(&stream, &create.to_string());
    assert_eq!(created["result"]["ok"], true, "{created}");
    assert_eq!(created["result"]["vms"][0]["name"], "lab");
    assert_eq!(created["result"]["vms"][0]["running"], false);

    let deleted = rpc(
        &stream,
        r#"{"jsonrpc":"2.0","method":"delete","params":{"name":"lab"},"id":4}"#,
    );
    assert_eq!(deleted["result"]["ok"], true, "{deleted}");
    // SAFETY: restore process env this test mutated; tests run with --test-threads=1.
    unsafe {
        match prev_vms {
            Some(value) => std::env::set_var(VMS_DIR_ENV, value),
            None => std::env::remove_var(VMS_DIR_ENV),
        }
        match prev_run {
            Some(value) => std::env::set_var(RUN_DIR_ENV, value),
            None => std::env::remove_var(RUN_DIR_ENV),
        }
    }
}

#[test]
fn vm_subscribe_emits_state() {
    let dir = Scratch::new("sub");
    let path = dir.0.join("vm.sock");
    let control = Arc::new(VmControl::new("subvm", 1));
    let _server = ControlServer::start(&path, control).expect("start");
    let stream = UnixStream::connect(&path).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("to");
    rpc(
        &stream,
        r#"{"jsonrpc":"2.0","method":"handshake","params":{"api_version":1},"id":1}"#,
    );
    let ack = rpc(
        &stream,
        r#"{"jsonrpc":"2.0","method":"subscribe","params":{"events":["state"]},"id":2}"#,
    );
    assert_eq!(ack["result"]["ok"], true);
    let mut line = String::new();
    BufReader::new(stream.try_clone().expect("r"))
        .read_line(&mut line)
        .expect("event");
    let event: serde_json::Value = serde_json::from_str(line.trim()).expect("json");
    assert_eq!(event["method"], "event");
    assert_eq!(event["params"]["kind"], "state");
    assert_eq!(event["params"]["vm"], "subvm");
}
