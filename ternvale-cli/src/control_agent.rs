//! Control-socket `exec` / `copy` (gated by `agent.allow_exec`).

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};
use ternvale_agent_proto::{encode_hex, is_guest_file_path, ExecStream, MAX_BIN_CHUNK};
use ternvale_devices::AgentServer;

use crate::protocol::{Request, Response};

const COPY_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_EXEC_MS: u64 = 60_000;
const MAX_EXEC_MS: u64 = 600_000;

/// Refuse unless `allow_exec` is set on this VM.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn gated(allow_exec: bool, agent: Option<&AgentServer>, request: &Request) -> Option<Response> {
    match request {
        Request::Exec { argv, timeout_ms } => {
            Some(exec(allow_exec, agent, argv, *timeout_ms, |_, _| {}))
        }
        Request::Copy {
            guest_path,
            host_path,
            to_guest,
            offset,
            limit,
        } => Some(copy(
            allow_exec,
            agent,
            guest_path,
            Path::new(host_path),
            *to_guest,
            *offset,
            *limit,
        )),
        _ => None,
    }
}

/// Run exec; `on_chunk` is called for each stdout/stderr piece (streaming).
#[tracing::instrument(level = "info", target = "ternvale::cli", skip_all, fields(argc = argv.len()))]
pub fn exec(
    allow_exec: bool,
    agent: Option<&AgentServer>,
    argv: &[String],
    timeout_ms: Option<u64>,
    mut on_chunk: impl FnMut(ExecStream, &[u8]),
) -> Response {
    tracing::info!(target: "ternvale::cli", argv = ?argv, "exec request");
    if !allow_exec {
        tracing::warn!(target: "ternvale::cli", "exec refused: agent.allow_exec is false");
        return Response::error("exec is disabled (set agent.allow_exec = true in the VM config)");
    }
    let Some(agent) = agent else {
        return Response::error("this vm has no guest agent server");
    };
    let wait = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_EXEC_MS).min(MAX_EXEC_MS));
    match agent.exec(argv, wait, |stream, data| on_chunk(stream, data)) {
        Ok(code) => Response::exec_exit(code),
        Err(error) => Response::error(format!("exec: {error}")),
    }
}

/// Host ↔ guest copy with SHA-256 and resume.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(to_guest, offset, limit)
)]
pub fn copy(
    allow_exec: bool,
    agent: Option<&AgentServer>,
    guest_path: &str,
    host_path: &Path,
    to_guest: bool,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Response {
    if !allow_exec {
        tracing::warn!(target: "ternvale::cli", "copy refused: agent.allow_exec is false");
        return Response::error("copy is disabled (set agent.allow_exec = true in the VM config)");
    }
    let Some(agent) = agent else {
        return Response::error("this vm has no guest agent server");
    };
    if !is_guest_file_path(guest_path) {
        return Response::error("guest path must be absolute without '..'");
    }
    match if to_guest {
        put(agent, guest_path, host_path, offset, limit)
    } else {
        get(agent, guest_path, host_path, offset, limit)
    } {
        Ok((bytes, Some(sha256), resumed)) => Response::copy_done(bytes, sha256, resumed),
        Ok((bytes, None, resumed)) => Response::copy_progress(bytes, resumed),
        Err(error) => Response::error(error),
    }
}

fn put(
    agent: &AgentServer,
    guest_path: &str,
    host_path: &Path,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<(u64, Option<String>, bool), String> {
    let mut file =
        File::open(host_path).map_err(|e| format!("open {}: {e}", host_path.display()))?;
    let total = file
        .metadata()
        .map_err(|e| format!("stat {}: {e}", host_path.display()))?
        .len();
    let start = match offset {
        Some(off) => off.min(total),
        None => match agent.copy_hash(guest_path, COPY_TIMEOUT) {
            Ok((size, _)) => {
                tracing::info!(
                    target: "ternvale::cli",
                    size,
                    total,
                    "copy resume from existing guest size"
                );
                size.min(total)
            }
            Err(error) => {
                tracing::debug!(
                    target: "ternvale::cli",
                    error = %error,
                    "copy guest file absent; starting at 0"
                );
                0
            }
        },
    };
    let end = limit
        .map(|n| start.saturating_add(n).min(total))
        .unwrap_or(total);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("seek {}: {e}", host_path.display()))?;
    let mut pos = start;
    let mut buf = vec![0u8; MAX_BIN_CHUNK];
    while pos < end {
        let want = ((end - pos) as usize).min(buf.len());
        let n = file
            .read(&mut buf[..want])
            .map_err(|e| format!("read {}: {e}", host_path.display()))?;
        if n == 0 {
            break;
        }
        let last = pos + n as u64 >= total;
        agent
            .copy_put(guest_path, pos, &buf[..n], last, COPY_TIMEOUT)
            .map_err(|e| e.to_string())?;
        pos += n as u64;
    }
    if pos == start && total == start {
        agent
            .copy_put(guest_path, pos, &[], true, COPY_TIMEOUT)
            .map_err(|e| e.to_string())?;
    }
    if pos < total {
        tracing::debug!(
            target: "ternvale::cli",
            pos,
            total,
            "copy host→guest slice (not hashed yet)"
        );
        return Ok((pos, None, start > 0));
    }
    let (size, sha256) = agent
        .copy_hash(guest_path, COPY_TIMEOUT)
        .map_err(|e| e.to_string())?;
    let host_hash = sha256_path(host_path)?;
    if size != total || sha256 != host_hash {
        return Err(format!(
            "checksum mismatch: guest {sha256} size {size}, host {host_hash} size {total}"
        ));
    }
    Ok((total, Some(sha256), start > 0))
}

fn get(
    agent: &AgentServer,
    guest_path: &str,
    host_path: &Path,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<(u64, Option<String>, bool), String> {
    let start = offset.unwrap_or(0);
    let end = limit.map(|n| start.saturating_add(n));
    if let Some(parent) = host_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
    }
    let mut file = if start == 0 {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(host_path)
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(host_path)
    }
    .map_err(|e| format!("open {}: {e}", host_path.display()))?;
    if start > 0 {
        let len = file
            .metadata()
            .map_err(|e| format!("stat {}: {e}", host_path.display()))?
            .len();
        if start > len {
            return Err(format!("resume offset {start} past host size {len}"));
        }
        file.seek(SeekFrom::Start(start))
            .map_err(|e| format!("seek {}: {e}", host_path.display()))?;
    }
    let mut pos = start;
    let mut eof = false;
    loop {
        if let Some(end) = end {
            if pos >= end {
                break;
            }
        }
        let want = match end {
            Some(end) => ((end - pos) as u32).min(MAX_BIN_CHUNK as u32),
            None => MAX_BIN_CHUNK as u32,
        };
        let (chunk, at_end) = agent
            .copy_get(guest_path, pos, want, COPY_TIMEOUT)
            .map_err(|e| e.to_string())?;
        file.write_all(&chunk)
            .map_err(|e| format!("write {}: {e}", host_path.display()))?;
        pos += chunk.len() as u64;
        eof = at_end;
        if at_end || chunk.is_empty() {
            break;
        }
    }
    if !eof {
        tracing::debug!(
            target: "ternvale::cli",
            pos,
            "copy guest→host slice (not hashed yet)"
        );
        return Ok((pos, None, start > 0));
    }
    file.set_len(pos)
        .map_err(|e| format!("truncate {}: {e}", host_path.display()))?;
    file.sync_all()
        .map_err(|e| format!("sync {}: {e}", host_path.display()))?;
    let (guest_size, guest_hash) = agent
        .copy_hash(guest_path, COPY_TIMEOUT)
        .map_err(|e| e.to_string())?;
    let host_hash = sha256_path(host_path)?;
    if guest_size != pos || guest_hash != host_hash {
        return Err(format!(
            "checksum mismatch: guest {guest_hash} size {guest_size}, host {host_hash} size {pos}"
        ));
    }
    Ok((pos, Some(guest_hash), start > 0))
}

fn sha256_path(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("hash {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("hash {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(encode_hex(hasher.finalize().as_slice()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_without_allow_exec() {
        let response = exec(false, None, &["echo".into()], None, |_, _| {});
        assert!(!response.ok);
        assert!(response
            .error
            .as_deref()
            .unwrap_or("")
            .contains("allow_exec"));
        let response = copy(false, None, "/tmp/a", Path::new("/tmp/b"), true, None, None);
        assert!(!response.ok);
    }
}
