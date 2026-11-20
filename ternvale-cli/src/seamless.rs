//! Experimental seamless windows: poll guest window geometry and publish JSON updates.
//!
//! Active when `TERNVALE_SEAMLESS_WINDOWS=1` (app toggle) or
//! `[input] seamless_windows = true`. The app reads `<run>/<vm>.seamless.sock`
//! (one JSON object per line) and may write host ops back on the same socket.
//! Missing agent or an empty window list keeps single-window display.

use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ternvale_agent_proto::GuestWindow;
use ternvale_config::VmConfig;
use ternvale_devices::{AgentServer, AgentState, WindowPixels};

const POLL: Duration = Duration::from_millis(500);
const AGENT_WAIT: Duration = Duration::from_secs(3);

/// Handle for the seamless publisher thread.
pub struct SeamlessPublisher {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    path: PathBuf,
}

/// True when env or `[input] seamless_windows` requests the mode.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = %config.name))]
pub fn is_enabled(config: &VmConfig) -> bool {
    let env = std::env::var_os("TERNVALE_SEAMLESS_WINDOWS").is_some();
    let cfg = config.input_section().seamless_windows;
    tracing::debug!(target: "ternvale::cli", env, cfg, "seamless windows gate");
    env || cfg
}

impl SeamlessPublisher {
    /// Start when gated on and an agent server is present.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, path = %path.display(), enabled))]
    pub fn start(
        vm: &str,
        path: PathBuf,
        agent: Arc<AgentServer>,
        enabled: bool,
    ) -> Result<Self, std::io::Error> {
        if !enabled {
            tracing::debug!(target: "ternvale::cli", vm, "seamless windows disabled");
            return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        }
        tracing::info!(
            target: "ternvale::cli",
            vm,
            path = %path.display(),
            "experimental seamless windows mode active"
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        clear_stale(&path)?;
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let vm_name = vm.to_string();
        let sock = path.clone();
        let thread = std::thread::Builder::new()
            .name("seamless-windows".into())
            .spawn(move || run_loop(&vm_name, listener, agent, flag, sock))
            .map_err(|source| {
                tracing::error!(target: "ternvale::cli", error = %source, "seamless publisher thread failed");
                source
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
            path,
        })
    }
}

impl Drop for SeamlessPublisher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::error!(target: "ternvale::cli", "seamless publisher thread panicked");
            }
        }
        match std::fs::remove_file(&self.path) {
            Ok(()) => {
                tracing::info!(target: "ternvale::cli", path = %self.path.display(), "seamless socket removed")
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(target: "ternvale::cli", path = %self.path.display(), error = %error, "could not remove seamless socket")
            }
        }
    }
}

#[derive(Serialize)]
struct Update<'a> {
    windows: &'a [GuestWindow],
    sequence: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    frames: Vec<FrameMeta>,
}

#[derive(Serialize, Clone)]
struct FrameMeta {
    id: u32,
    seq: u64,
    width: u32,
    height: u32,
    format: String,
    path: String,
}

#[derive(Deserialize, Debug)]
struct HostLine {
    op: String,
    id: u32,
    #[serde(default)]
    x: Option<i32>,
    #[serde(default)]
    y: Option<i32>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    code: Option<u32>,
    #[serde(default)]
    value: Option<i32>,
}

struct Client {
    stream: UnixStream,
    buf: Vec<u8>,
}

fn run_loop(
    vm: &str,
    listener: UnixListener,
    agent: Arc<AgentServer>,
    stop: Arc<AtomicBool>,
    sock: PathBuf,
) {
    tracing::debug!(target: "ternvale::cli", vm, "seamless publisher started");
    let frame_dir = sock.with_extension("frames");
    let mut sequence = 0u64;
    let mut last_json = String::new();
    let mut clients: Vec<Client> = Vec::new();
    let mut last_empty_warn = false;
    while !stop.load(Ordering::Acquire) {
        accept_clients(&listener, &mut clients, &last_json);
        drain_host_ops(&mut clients, &agent);
        if agent.status().state == AgentState::Connected {
            match agent.window_list_get(AGENT_WAIT) {
                Ok(windows) => {
                    last_empty_warn = false;
                    sequence = sequence.wrapping_add(1);
                    let frames = persist_frames(&frame_dir, agent.take_window_frames());
                    let update = Update {
                        windows: &windows,
                        sequence,
                        frames,
                    };
                    if let Ok(json) = serde_json::to_string(&update) {
                        if json != last_json {
                            tracing::info!(
                                target: "ternvale::cli",
                                vm,
                                count = windows.len(),
                                sequence,
                                ids = ?windows.iter().map(|w| w.id).collect::<Vec<_>>(),
                                "seamless window list updated"
                            );
                            last_json = json.clone();
                            push_json(&mut clients, &json);
                        }
                    }
                }
                Err(error) => {
                    if !last_empty_warn {
                        tracing::warn!(
                            target: "ternvale::cli",
                            vm,
                            error = %error,
                            "seamless window list unavailable; keeping single-window mode"
                        );
                        last_empty_warn = true;
                    }
                }
            }
        } else if !last_empty_warn {
            tracing::info!(
                target: "ternvale::cli",
                vm,
                "seamless waiting for guest agent (single-window fallback)"
            );
            last_empty_warn = true;
        }
        std::thread::sleep(POLL);
    }
    tracing::debug!(target: "ternvale::cli", vm, "seamless publisher stopped");
}

fn persist_frames(dir: &Path, frames: Vec<WindowPixels>) -> Vec<FrameMeta> {
    if frames.is_empty() {
        return Vec::new();
    }
    if let Err(error) = std::fs::create_dir_all(dir) {
        tracing::warn!(target: "ternvale::cli", error = %error, "seamless frame dir");
        return Vec::new();
    }
    let mut meta = Vec::new();
    for frame in frames {
        let path = dir.join(format!("{}.rgb24", frame.id));
        match std::fs::write(&path, &frame.data) {
            Ok(()) => {
                tracing::debug!(
                    target: "ternvale::cli",
                    id = frame.id,
                    seq = frame.seq,
                    bytes = frame.data.len(),
                    "seamless frame written (pixels not logged)"
                );
                meta.push(FrameMeta {
                    id: frame.id,
                    seq: frame.seq,
                    width: frame.width,
                    height: frame.height,
                    format: frame.format,
                    path: path.display().to_string(),
                });
            }
            Err(error) => {
                tracing::warn!(target: "ternvale::cli", error = %error, "seamless frame write failed")
            }
        }
    }
    meta
}

fn drain_host_ops(clients: &mut [Client], agent: &AgentServer) {
    for client in clients.iter_mut() {
        let mut tmp = [0u8; 512];
        loop {
            match client.stream.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => client.buf.extend_from_slice(&tmp[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
            if client.buf.len() > 64 * 1024 {
                client.buf.clear();
                tracing::warn!(target: "ternvale::cli", "seamless host line too long; dropped");
                break;
            }
        }
        while let Some(idx) = client.buf.iter().position(|b| *b == b'\n') {
            let line = client.buf.drain(..=idx).collect::<Vec<_>>();
            let text = String::from_utf8_lossy(&line);
            apply_host_line(agent, text.trim());
        }
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(line = line))]
fn apply_host_line(agent: &AgentServer, line: &str) {
    if line.is_empty() {
        return;
    }
    let Ok(cmd) = serde_json::from_str::<HostLine>(line) else {
        tracing::warn!(target: "ternvale::cli", line, "seamless host line ignored");
        return;
    };
    let input_kind = cmd.kind.as_deref().unwrap_or(cmd.op.as_str());
    if matches!(input_kind, "key" | "button" | "move" | "abs") {
        if let Err(error) = agent.window_input(
            cmd.id,
            input_kind,
            cmd.code.unwrap_or(0),
            cmd.value.unwrap_or(0),
        ) {
            tracing::debug!(target: "ternvale::cli", error = %error, "window_input not sent");
        }
        return;
    }
    if let Err(error) = agent.window_op(cmd.id, &cmd.op, cmd.x, cmd.y, cmd.width, cmd.height) {
        tracing::debug!(target: "ternvale::cli", error = %error, "window_op not sent");
    }
}

fn accept_clients(listener: &UnixListener, clients: &mut Vec<Client>, last_json: &str) {
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(true);
                tracing::debug!(target: "ternvale::cli", "seamless app client connected");
                if !last_json.is_empty() {
                    let _ = write_line(&mut stream, last_json);
                }
                clients.push(Client {
                    stream,
                    buf: Vec::new(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => {
                tracing::warn!(target: "ternvale::cli", error = %error, "seamless accept failed");
                break;
            }
        }
    }
}

fn push_json(clients: &mut Vec<Client>, json: &str) {
    clients.retain_mut(|client| match write_line(&mut client.stream, json) {
        Ok(()) => true,
        Err(error) => {
            tracing::debug!(target: "ternvale::cli", error = %error, "seamless client dropped");
            false
        }
    });
}

fn write_line(stream: &mut UnixStream, json: &str) -> std::io::Result<()> {
    writeln!(stream, "{json}")?;
    stream.flush()
}

fn clear_stale(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(meta) if !meta.file_type().is_socket() => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "seamless path exists and is not a socket",
        )),
        Ok(_) if UnixStream::connect(path).is_ok() => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "seamless socket in use",
        )),
        Ok(_) => std::fs::remove_file(path),
    }
}

#[cfg(test)]
#[path = "seamless_quality.rs"]
mod quality;

#[cfg(test)]
mod tests {
    use super::*;
    use ternvale_agent_proto::GuestWindow;

    #[test]
    fn update_json_changes_when_one_of_two_windows_closes() {
        let two = vec![
            GuestWindow {
                id: 1,
                title: "A".into(),
                x: 0,
                y: 0,
                width: 400,
                height: 300,
                ..GuestWindow::default()
            },
            GuestWindow {
                id: 2,
                title: "B".into(),
                x: 500,
                y: 0,
                width: 400,
                height: 300,
                ..GuestWindow::default()
            },
        ];
        let one = vec![two[0].clone()];
        let json_two = serde_json::to_string(&Update {
            windows: &two,
            sequence: 1,
            frames: Vec::new(),
        })
        .expect("encode");
        let json_one = serde_json::to_string(&Update {
            windows: &one,
            sequence: 2,
            frames: Vec::new(),
        })
        .expect("encode");
        assert_ne!(json_two, json_one);
        assert!(json_two.contains("\"id\":2"));
        assert!(!json_one.contains("\"id\":2"));
        assert!(!json_two.contains("frames"));
    }

    #[test]
    fn update_json_changes_when_window_moves() {
        let a = GuestWindow {
            id: 10,
            title: "term".into(),
            x: 0,
            y: 0,
            width: 640,
            height: 480,
            ..GuestWindow::default()
        };
        let b = GuestWindow {
            x: 120,
            ..a.clone()
        };
        let ja = serde_json::to_string(&Update {
            windows: std::slice::from_ref(&a),
            sequence: 1,
            frames: Vec::new(),
        })
        .expect("encode");
        let jb = serde_json::to_string(&Update {
            windows: std::slice::from_ref(&b),
            sequence: 2,
            frames: Vec::new(),
        })
        .expect("encode");
        assert_ne!(ja, jb);
    }

    #[test]
    fn host_line_parses_focus_and_input() {
        let focus: HostLine = serde_json::from_str(r#"{"op":"focus","id":9}"#).expect("focus");
        assert_eq!(focus.op, "focus");
        assert_eq!(focus.id, 9);
        let abs: HostLine =
            serde_json::from_str(r#"{"op":"abs","id":9,"kind":"abs","code":0,"value":12}"#)
                .expect("abs");
        assert_eq!(abs.kind.as_deref(), Some("abs"));
    }
}
