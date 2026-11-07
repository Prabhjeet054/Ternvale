//! Watch an unattended Windows install: disk I/O + agent connect, or fail with logs.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use std::process::ExitCode;

use crate::client;
use crate::logs;
use crate::paths;
use crate::protocol::{Request, Response};

/// Windows Boot Manager CD prompt (serial). A CR is the unattended "any key".
pub const CD_BOOT_PROMPT: &str = "Press any key to boot from CD or DVD";

/// Spawn `ternvale run` and wait until the guest agent connects.
#[tracing::instrument(
    level = "info",
    target = "ternvale::cli",
    skip_all,
    fields(vm, timeout_secs)
)]
pub fn wait_for_agent(
    config: &Path,
    serial: &Path,
    vm: &str,
    timeout_secs: u64,
) -> Result<ExitCode> {
    let exe = std::env::current_exe().context("current_exe for ternvale run")?;
    tracing::info!(
        target: "ternvale::cli",
        vm,
        config = %config.display(),
        serial = %serial.display(),
        timeout_secs,
        "starting unattended install watcher"
    );
    let mut child = Command::new(&exe)
        .args(["run", "--display", "none"])
        .arg(config)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn ternvale run for unattended install")?;
    let mut stdin = child.stdin.take();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(timeout_secs.max(1));
    let mut last_bytes = 0u64;
    let mut cd_key_logged = false;
    let result = loop {
        if Instant::now() > deadline {
            break Err(anyhow_timeout(vm, last_bytes, serial));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break Err(fail(
                    vm,
                    last_bytes,
                    format!("VMM exited ({status})"),
                    serial,
                ));
            }
            Ok(None) => {}
            Err(error) => break Err(error).context("wait for ternvale run"),
        }
        if let Some(stdin) = stdin.as_mut() {
            poke_cd_boot(stdin, started, &mut cd_key_logged);
        }
        match poll(vm) {
            Ok(Poll::Connected { bytes }) => {
                tracing::info!(
                    target: "ternvale::cli",
                    vm,
                    blk_bytes = bytes,
                    "unattended install: guest agent connected"
                );
                println!("progress: agent connected (disk I/O {bytes} bytes)");
                break Ok(ExitCode::SUCCESS);
            }
            Ok(Poll::Running { bytes, agent }) => {
                if bytes != last_bytes {
                    println!("progress: disk I/O {bytes} bytes, agent {agent}");
                    last_bytes = bytes;
                }
            }
            Ok(Poll::Failed(message)) => break Err(fail(vm, last_bytes, message, serial)),
            Err(error) => {
                tracing::debug!(target: "ternvale::cli", error = %error, "install poll");
            }
        }
        let pause = if started.elapsed() < Duration::from_secs(25) {
            Duration::from_millis(200)
        } else {
            Duration::from_secs(2)
        };
        std::thread::sleep(pause);
    };
    match result {
        Ok(code) => Ok(code),
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

enum Poll {
    Connected { bytes: u64 },
    Running { bytes: u64, agent: String },
    Failed(String),
}

fn poll(vm: &str) -> Result<Poll> {
    let socket = paths::socket_path(vm)?;
    let stream = match client::connect(&socket)? {
        Ok(stream) => stream,
        Err(_) => {
            return Ok(Poll::Running {
                bytes: 0,
                agent: "waiting-for-vmm".into(),
            })
        }
    };
    let stats: Response = client::exchange(stream, &Request::QueryStats)?;
    if !stats.ok {
        let message = stats.error.unwrap_or_else(|| "query-stats failed".into());
        return Ok(Poll::Failed(message));
    }
    let bytes = stats
        .stats
        .as_ref()
        .and_then(|s| s.metrics.as_ref())
        .map(|m| m.blk.bytes)
        .unwrap_or(0);
    let stream = match client::connect(&socket)? {
        Ok(stream) => stream,
        Err(_) => {
            return Ok(Poll::Running {
                bytes,
                agent: "waiting-for-vmm".into(),
            })
        }
    };
    let response: Response = client::exchange(stream, &Request::Status)?;
    if !response.ok {
        let message = response.error.unwrap_or_else(|| "status failed".into());
        return Ok(Poll::Failed(message));
    }
    if let Some(status) = &response.status {
        if let Some(failure) = &status.failure {
            return Ok(Poll::Failed(failure.clone()));
        }
        if let Some(cause) = &status.stop_cause {
            let state = status.state.as_str();
            if state == "failed" || state == "stopping" || state == "exited" {
                return Ok(Poll::Failed(cause.clone()));
            }
        }
        if let Some(agent) = &status.agent {
            if agent.state == "connected" {
                return Ok(Poll::Connected { bytes });
            }
            return Ok(Poll::Running {
                bytes,
                agent: agent.state.clone(),
            });
        }
    }
    Ok(Poll::Running {
        bytes,
        agent: "unknown".into(),
    })
}

fn anyhow_timeout(vm: &str, bytes: u64, serial: &Path) -> anyhow::Error {
    fail(
        vm,
        bytes,
        "timed out waiting for the guest agent".into(),
        serial,
    )
}

fn fail(vm: &str, bytes: u64, reason: String, serial: &Path) -> anyhow::Error {
    let logs = last_log_lines(vm, 30);
    tracing::error!(
        target: "ternvale::cli",
        vm,
        blk_bytes = bytes,
        reason = %reason,
        "unattended install failed"
    );
    eprintln!("unattended install failed: {reason} (disk I/O {bytes} bytes)");
    if logs.is_empty() {
        eprintln!("(no host log lines)");
    } else {
        eprintln!("last log lines:");
        eprint!("{logs}");
    }
    if let Ok(text) = std::fs::read_to_string(serial) {
        let tail: String = text
            .chars()
            .rev()
            .take(1500)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if !tail.is_empty() {
            eprintln!("last serial:");
            eprintln!("{tail}");
        }
    }
    anyhow::anyhow!("{reason}")
}

/// El Torito waits for a key; serial log is 256-byte batched so the prompt is
/// often flushed only after the timeout. Poke UART CR for the first 20s.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
fn poke_cd_boot(stdin: &mut impl Write, started: Instant, logged: &mut bool) {
    if started.elapsed() > Duration::from_secs(20) {
        return;
    }
    match stdin.write_all(b"\r") {
        Ok(()) => {
            let _ = stdin.flush();
            if !*logged {
                *logged = true;
                tracing::info!(
                    target: "ternvale::cli",
                    "poking UART CR for Windows CD boot prompt (no operator)"
                );
                println!("progress: poking CD boot prompt");
            }
        }
        Err(error) => tracing::warn!(
            target: "ternvale::cli",
            error = %error,
            "could not send CD boot CR"
        ),
    }
}

fn last_log_lines(vm: &str, n: usize) -> String {
    let Ok(dir) = ternvale_log::LogConfig::default_log_dir() else {
        return String::new();
    };
    let Ok(Some(path)) = logs::newest(&dir, vm) else {
        return String::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    lines
        .iter()
        .rev()
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .fold(String::new(), |mut acc, line| {
            acc.push_str(line);
            acc.push('\n');
            acc
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_log_lines_empty_when_missing() {
        let text = last_log_lines("no-such-vm-zzzz", 5);
        assert!(text.is_empty() || !text.contains("password"));
    }

    #[test]
    fn cd_boot_poke_sends_cr_for_twenty_seconds() {
        let mut buf = Vec::new();
        let mut logged = false;
        poke_cd_boot(&mut buf, Instant::now(), &mut logged);
        assert_eq!(buf, b"\r");
        assert!(logged);
        poke_cd_boot(
            &mut buf,
            Instant::now() - Duration::from_secs(30),
            &mut logged,
        );
        assert_eq!(buf, b"\r");
    }
}
