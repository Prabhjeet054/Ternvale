//! `ternvale --remote https://…` talks to `ternvaled`, not local sockets.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use ternvale_gateway::{GatewayClient, TokenSecret};

use crate::cli::{Command, SnapshotCommand, VmCommand};

/// Dispatch a subset of CLI commands over the HTTPS gateway.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn dispatch(
    remote: &str,
    token: Option<String>,
    tls_ca: Option<PathBuf>,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
    command: Command,
    json: bool,
) -> Result<ExitCode> {
    let ca = tls_ca.unwrap_or_else(default_ca);
    let secret = token.map(TokenSecret::new);
    let client =
        GatewayClient::connect(remote, secret, &ca, tls_cert.as_deref(), tls_key.as_deref())
            .context("connect to remote gateway")?;
    tracing::info!(
        target: "ternvale::cli",
        remote,
        ca = %ca.display(),
        "remote gateway"
    );
    let (method, path, body) = map_command(&command)?;
    let value = client
        .json(method, &path, body.as_ref())
        .context("gateway request")?;
    emit(json, &value)?;
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        bail!(
            "{}",
            value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("gateway request failed")
        );
    }
    Ok(ExitCode::SUCCESS)
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
fn map_command(command: &Command) -> Result<(&'static str, String, Option<Value>)> {
    match command {
        Command::List => Ok(("GET", "/v1/vms".into(), None)),
        Command::Start { name } => Ok(("POST", format!("/v1/vms/{name}/start"), Some(json!({})))),
        Command::Status { name, .. } | Command::Stats { name, .. } => {
            Ok(("GET", format!("/v1/vms/{name}/stats"), None))
        }
        Command::Pause { name, timeout_ms } => Ok((
            "POST",
            format!("/v1/vms/{name}/pause"),
            Some(json!({ "timeout_ms": timeout_ms })),
        )),
        Command::Resume { name } => Ok(("POST", format!("/v1/vms/{name}/resume"), Some(json!({})))),
        Command::Stop {
            name,
            force,
            graceful,
            ..
        } => Ok((
            "POST",
            format!("/v1/vms/{name}/stop"),
            Some(json!({ "force": force, "graceful": graceful })),
        )),
        Command::Snapshot { action } => snapshot(action),
        Command::Clone { source, new_name } => Ok((
            "POST",
            format!("/v1/vms/{source}/clone"),
            Some(json!({ "new_name": new_name })),
        )),
        Command::Vm {
            action: VmCommand::Delete { name },
        } => Ok(("DELETE", format!("/v1/vms/{name}"), None)),
        Command::Balloon {
            name,
            target_mib,
            auto,
        } => {
            let mut body = serde_json::Map::new();
            if let Some(mib) = target_mib {
                body.insert("target_mib".into(), json!(mib));
            }
            if *auto {
                body.insert("auto".into(), json!(true));
            }
            Ok((
                "POST",
                format!("/v1/vms/{name}/balloon"),
                Some(Value::Object(body)),
            ))
        }
        Command::Serve | Command::Run { .. } => {
            bail!(
                "--remote cannot run `serve` or `run`; start VMs with `ternvale --remote … start`"
            )
        }
        _ => bail!("this command is not supported with --remote (see docs/GATEWAY.md)"),
    }
}

fn snapshot(action: &SnapshotCommand) -> Result<(&'static str, String, Option<Value>)> {
    match action {
        SnapshotCommand::List { vm } => Ok(("GET", format!("/v1/vms/{vm}/snapshots"), None)),
        SnapshotCommand::Create { vm, name } => Ok((
            "POST",
            format!("/v1/vms/{vm}/snapshots"),
            Some(json!({ "snapshot": name })),
        )),
        SnapshotCommand::Restore { vm, name, .. } => Ok((
            "POST",
            format!("/v1/vms/{vm}/snapshots/{name}/restore"),
            None,
        )),
        SnapshotCommand::Delete { vm, name } => {
            Ok(("DELETE", format!("/v1/vms/{vm}/snapshots/{name}"), None))
        }
    }
}

fn emit(json_mode: bool, value: &Value) -> Result<()> {
    crate::json_out::emit(json_mode, value, || {
        println!("{}", value);
    })
}

fn default_ca() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => {
            PathBuf::from(home).join("Library/Application Support/Ternvale/gateway/ca.pem")
        }
        None => PathBuf::from("ca.pem"),
    }
}
