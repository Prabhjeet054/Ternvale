//! Experimental seamless windows: poll guest window geometry and publish JSON updates.
//!
//! Active when `TERNVALE_SEAMLESS_WINDOWS=1` (set by the macOS app settings toggle).
//! The app reads `<run>/<vm>.seamless.sock` (one JSON object per line).

use std::io::{BufWriter, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Serialize;
use ternvale_agent_proto::GuestWindow;
use ternvale_devices::{AgentServer, AgentState};

const POLL: Duration = Duration::from_millis(500);
const AGENT_WAIT: Duration = Duration::from_secs(3);

/// Handle for the seamless publisher thread.
pub struct SeamlessPublisher {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    path: PathBuf,
}

impl SeamlessPublisher {
    /// Start when the env var is set and an agent server is present.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, path = %path.display()))]
    pub fn start(vm: &str, path: PathBuf, agent: Arc<AgentServer>) -> Result<Self, std::io::Error> {
        if std::env::var_os("TERNVALE_SEAMLESS_WINDOWS").is_none() {
            tracing::debug!(target: "ternvale::cli", vm, "seamless windows disabled (no TERNVALE_SEAMLESS_WINDOWS)");
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
        let thread = std::thread::Builder::new()
            .name("seamless-windows".into())
            .spawn(move || run_loop(&vm_name, listener, agent, flag))
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
}

fn run_loop(vm: &str, listener: UnixListener, agent: Arc<AgentServer>, stop: Arc<AtomicBool>) {
    tracing::debug!(target: "ternvale::cli", vm, "seamless publisher started");
    let mut sequence = 0u64;
    let mut last_json = String::new();
    let mut clients: Vec<UnixStream> = Vec::new();
    while !stop.load(Ordering::Acquire) {
        accept_clients(&listener, &mut clients, &last_json);
        if agent.status().state == AgentState::Connected {
            if let Ok(windows) = agent.window_list_get(AGENT_WAIT) {
                sequence = sequence.wrapping_add(1);
                let update = Update {
                    windows: &windows,
                    sequence,
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
        }
        std::thread::sleep(POLL);
    }
    tracing::debug!(target: "ternvale::cli", vm, "seamless publisher stopped");
}

fn accept_clients(listener: &UnixListener, clients: &mut Vec<UnixStream>, last_json: &str) {
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                tracing::debug!(target: "ternvale::cli", "seamless app client connected");
                if !last_json.is_empty() {
                    let _ = write_line(&mut stream, last_json);
                }
                clients.push(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => {
                tracing::warn!(target: "ternvale::cli", error = %error, "seamless accept failed");
                break;
            }
        }
    }
}

fn push_json(clients: &mut Vec<UnixStream>, json: &str) {
    clients.retain_mut(|stream| match write_line(stream, json) {
        Ok(()) => true,
        Err(error) => {
            tracing::debug!(target: "ternvale::cli", error = %error, "seamless client dropped");
            false
        }
    });
}

fn write_line(stream: &mut UnixStream, json: &str) -> std::io::Result<()> {
    let mut writer = BufWriter::new(stream);
    writeln!(writer, "{json}")?;
    writer.flush()
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
            },
            GuestWindow {
                id: 2,
                title: "B".into(),
                x: 500,
                y: 0,
                width: 400,
                height: 300,
            },
        ];
        let one = vec![two[0].clone()];
        let json_two = serde_json::to_string(&Update {
            windows: &two,
            sequence: 1,
        })
        .expect("encode");
        let json_one = serde_json::to_string(&Update {
            windows: &one,
            sequence: 2,
        })
        .expect("encode");
        assert_ne!(json_two, json_one);
        assert!(json_two.contains("\"id\":2"));
        assert!(!json_one.contains("\"id\":2"));
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
        };
        let b = GuestWindow {
            x: 120,
            ..a.clone()
        };
        let ja = serde_json::to_string(&Update {
            windows: std::slice::from_ref(&a),
            sequence: 1,
        })
        .expect("encode");
        let jb = serde_json::to_string(&Update {
            windows: std::slice::from_ref(&b),
            sequence: 2,
        })
        .expect("encode");
        assert_ne!(ja, jb);
    }
}
