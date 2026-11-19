//! `ternvale driver-logs <vm> [--out FILE] [--timeout S] [--max-bytes N]`

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::client;
use crate::json_out;
use crate::paths;
use crate::protocol::Request;

/// Pull concatenated guest driver sidecar logs (not gated by `allow_exec`).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name, timeout_s = timeout))]
pub fn driver_logs(
    name: &str,
    out: Option<&Path>,
    timeout: u64,
    max_bytes: Option<u32>,
    json: bool,
) -> Result<ExitCode> {
    let socket = paths::socket_path(name)?;
    let timeout_ms = timeout.saturating_mul(1000).max(1);
    let request = Request::DriverLogs {
        timeout_ms: Some(timeout_ms),
        max_bytes,
    };
    let stream = match client::connect(&socket)? {
        Ok(stream) => stream,
        Err(client::Reach::NotRunning) => {
            bail!("cannot pull driver logs from vm {name}: it is not running")
        }
        Err(client::Reach::Stale) => {
            bail!("cannot pull driver logs from vm {name}: stale control socket")
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_millis(
            timeout_ms.saturating_add(5_000),
        )))
        .context("set driver-logs read timeout")?;
    let response =
        client::exchange(stream, &request).with_context(|| format!("driver-logs for vm {name}"))?;
    if !response.ok {
        let error = response.error.as_deref().unwrap_or("driver-logs failed");
        json_out::emit(
            json,
            serde_json::json!({"ok": false, "error": error}),
            || {},
        )?;
        bail!("vm {name} refused driver-logs: {error}");
    }
    let text = response.driver_logs.clone().unwrap_or_default();
    let truncated = response.driver_logs_truncated.unwrap_or(false);
    tracing::info!(
        target: "ternvale::cli",
        vm = name,
        bytes = text.len(),
        truncated,
        "driver logs pulled"
    );
    if json {
        json_out::emit(json, serde_json::to_value(&response)?, || {})?;
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(path) = out {
        std::fs::write(path, &text)
            .with_context(|| format!("write driver logs to {}", path.display()))?;
        println!(
            "wrote {} bytes to {}{}",
            text.len(),
            path.display(),
            if truncated { " (truncated)" } else { "" }
        );
    } else {
        print!("{text}");
        if truncated {
            eprintln!("(truncated)");
        }
    }
    Ok(ExitCode::SUCCESS)
}
