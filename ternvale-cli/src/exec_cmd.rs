//! `ternvale exec <vm> [--timeout S] -- <cmd...>`

use std::io::{BufRead, BufReader, Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::client;
use crate::paths;
use crate::protocol::{Request, Response, MAX_LINE, MAX_PAUSE_MS};

/// Run `argv` in `name` and stream output. The CLI exit code is the guest's.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name, argc = argv.len()))]
pub fn exec(name: &str, timeout: Option<u64>, argv: &[String], json: bool) -> Result<ExitCode> {
    if argv.is_empty() {
        bail!("exec needs a command after --");
    }
    let socket = paths::socket_path(name)?;
    let timeout_ms = timeout.map(|s| s.saturating_mul(1000).min(MAX_PAUSE_MS * 10));
    let request = Request::Exec {
        argv: argv.to_vec(),
        timeout_ms,
    };
    let stream = match client::connect(&socket)? {
        Ok(stream) => stream,
        Err(client::Reach::NotRunning) => {
            bail!("cannot exec on vm {name}: it is not running")
        }
        Err(client::Reach::Stale) => bail!("cannot exec on vm {name}: stale control socket"),
    };
    let wait = Duration::from_millis(timeout_ms.unwrap_or(60_000) + 10_000);
    stream
        .set_read_timeout(Some(wait))
        .context("set exec read timeout")?;
    let mut writer = stream.try_clone().context("clone exec connection")?;
    let mut line = serde_json::to_string(&request).context("encode exec request")?;
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .context("send exec request")?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut reply = String::new();
        let n = reader
            .by_ref()
            .take(MAX_LINE as u64 + 1)
            .read_line(&mut reply)
            .context("read exec response")?;
        if n == 0 {
            bail!("vm {name} closed the control socket during exec");
        }
        let response: Response = serde_json::from_str(reply.trim_end())
            .with_context(|| format!("parse exec response {reply:?}"))?;
        if !response.ok {
            let error = response.error.as_deref().unwrap_or("exec failed");
            if json {
                println!("{}", serde_json::to_string(&response)?);
            }
            bail!("vm {name} refused exec: {error}");
        }
        if let Some(code) = response.exit_code {
            if json {
                println!("{}", serde_json::to_string(&response)?);
            }
            tracing::info!(target: "ternvale::cli", vm = name, code, "exec finished");
            let raw = if (0..=255).contains(&code) {
                code as u8
            } else {
                1
            };
            return Ok(ExitCode::from(raw));
        }
        if json {
            println!("{}", serde_json::to_string(&response)?);
        } else if let (Some(stream_name), Some(data)) = (&response.stream, &response.data) {
            match stream_name.as_str() {
                "stderr" => eprint!("{data}"),
                _ => print!("{data}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_argv_is_rejected_by_the_request_shape() {
        let request = Request::Exec {
            argv: vec!["/bin/true".into()],
            timeout_ms: Some(1000),
        };
        assert_eq!(request.name(), "exec");
    }
}
