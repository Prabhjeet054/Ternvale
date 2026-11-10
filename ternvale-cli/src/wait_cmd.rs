//! `ternvale wait-ready <vm> --timeout`

use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::client;
use crate::json_out;
use crate::paths;
use crate::protocol::Request;

/// Poll until the guest agent is connected, or `timeout` seconds elapse.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name, timeout_s = timeout))]
pub fn wait_ready(name: &str, timeout: u64, json: bool) -> Result<ExitCode> {
    let socket = paths::socket_path(name)?;
    let deadline = Instant::now() + Duration::from_secs(timeout.max(1));
    loop {
        match client::connect(&socket)? {
            Err(reach) => {
                if Instant::now() >= deadline {
                    let msg = format!("vm {name} is not running ({reach:?})");
                    json_out::emit(json, serde_json::json!({"ok": false, "error": msg}), || {})?;
                    bail!("{msg}");
                }
            }
            Ok(stream) => {
                let response = client::exchange(stream, &Request::Status)
                    .with_context(|| format!("status vm {name}"))?;
                let ready = response
                    .status
                    .as_ref()
                    .and_then(|s| s.agent.as_ref())
                    .is_some_and(|a| a.state == "connected");
                if ready {
                    tracing::info!(target: "ternvale::cli", vm = name, "agent ready");
                    json_out::emit(
                        json,
                        serde_json::json!({"ok": true, "vm": name, "ready": true}),
                        || println!("vm {name}: agent connected"),
                    )?;
                    return Ok(ExitCode::SUCCESS);
                }
                if Instant::now() >= deadline {
                    let msg = format!("vm {name} agent did not connect within {timeout}s");
                    json_out::emit(json, serde_json::json!({"ok": false, "error": msg}), || {})?;
                    bail!("{msg}");
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
