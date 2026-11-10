//! Guest-side `Exec`: spawn a process and stream stdout/stderr.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_agent_proto::{write_message, ExecStream, HexBytes, Message, MAX_BIN_CHUNK};

use crate::{AgentError, Stream};

/// Run `argv` and write `ExecOutput` / `ExecExit` on `writer`.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id, argc = argv.len()))]
pub fn run(writer: Arc<Mutex<Stream>>, id: u64, argv: Vec<String>, timeout_ms: Option<u64>) {
    tracing::info!(target: "ternvale::agent", id, argv = ?argv, timeout_ms, "exec request");
    let code = match spawn(id, &argv, timeout_ms, &writer) {
        Ok(code) => code,
        Err(error) => {
            tracing::warn!(target: "ternvale::agent", id, error = %error, "exec failed");
            let _ = send(
                &writer,
                &Message::ExecOutput {
                    id,
                    stream: ExecStream::Stderr,
                    data: HexBytes(format!("{error}\n").into_bytes()),
                },
            );
            127
        }
    };
    let _ = send(&writer, &Message::ExecExit { id, code });
}

#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id))]
fn spawn(
    id: u64,
    argv: &[String],
    timeout_ms: Option<u64>,
    writer: &Arc<Mutex<Stream>>,
) -> Result<i32, AgentError> {
    let program = argv.first().ok_or_else(|| AgentError::Action {
        action: "exec",
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty argv"),
    })?;
    let mut child = Command::new(program)
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| AgentError::Action {
            action: "exec spawn",
            source,
        })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_writer = Arc::clone(writer);
    let err_writer = Arc::clone(writer);
    let out_thread = std::thread::spawn(move || drain(id, ExecStream::Stdout, stdout, &out_writer));
    let err_thread = std::thread::spawn(move || drain(id, ExecStream::Stderr, stderr, &err_writer));
    let deadline =
        Instant::now() + Duration::from_millis(timeout_ms.unwrap_or(60_000).min(600_000));
    let status = loop {
        if Instant::now() >= deadline {
            tracing::warn!(target: "ternvale::agent", id, "exec timed out; killing");
            let _ = child.kill();
            break child.wait().map_err(|source| AgentError::Action {
                action: "exec wait",
                source,
            })?;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(source) => {
                return Err(AgentError::Action {
                    action: "exec wait",
                    source,
                });
            }
        }
    };
    let _ = out_thread.join();
    let _ = err_thread.join();
    Ok(if Instant::now() >= deadline {
        124
    } else {
        exit_code(status)
    })
}

fn drain(
    id: u64,
    stream: ExecStream,
    pipe: Option<impl Read>,
    writer: &Arc<Mutex<Stream>>,
) -> Result<(), AgentError> {
    let Some(mut pipe) = pipe else {
        return Ok(());
    };
    let mut buf = vec![0u8; MAX_BIN_CHUNK.min(8192)];
    loop {
        let n = match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(AgentError::Action {
                    action: "exec read",
                    source,
                })
            }
        };
        send(
            writer,
            &Message::ExecOutput {
                id,
                stream,
                data: HexBytes(buf[..n].to_vec()),
            },
        )?;
    }
    Ok(())
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

fn send(writer: &Arc<Mutex<Stream>>, message: &Message) -> Result<(), AgentError> {
    let mut guard = match writer.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    write_message(&mut *guard, message, "host").map_err(|source| AgentError::Frame {
        op: "send exec",
        source,
    })
}
