//! Control socket server inside `ternvale run`.
//!
//! The listener polls `accept` every 50 ms so it can stop without a
//! self-connect. Each connection gets its own thread and may send several
//! requests, one JSON line each. The socket is mode 0600 in a 0700 directory
//! and is removed when the server stops.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ternvale_vmm::VmControl;

use crate::protocol::{Request, Response, DEFAULT_PAUSE_MS, MAX_LINE, MAX_PAUSE_MS};

const ACCEPT_POLL: Duration = Duration::from_millis(50);
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// A running control socket. Dropping it stops the listener and removes the socket.
pub struct ControlServer {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ControlServer {
    /// Bind `path` and serve `control`. Fails if another live VM owns the
    /// socket; a stale socket file left by a crash is replaced.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display(), vm = control.name()))]
    pub fn start(path: &Path, control: Arc<VmControl>) -> Result<Self> {
        let dir = path.parent().with_context(|| {
            format!("control socket {} has no parent directory", path.display())
        })?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create run directory {}", dir.display()))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restrict run directory {}", dir.display()))?;
        clear_stale(path)?;
        let listener = UnixListener::bind(path)
            .with_context(|| format!("bind control socket {}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restrict control socket {}", path.display()))?;
        listener
            .set_nonblocking(true)
            .with_context(|| format!("make control socket {} non-blocking", path.display()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("control".to_string())
            .spawn(move || accept_loop(&listener, &control, &flag))
            .context("spawn control socket thread")?;
        tracing::info!(target: "ternvale::cli", path = %path.display(), "control socket listening");
        Ok(Self {
            path: path.to_path_buf(),
            stop,
            thread: Some(thread),
        })
    }

    /// Socket path.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::error!(target: "ternvale::cli", "control socket thread panicked");
            }
        }
        match std::fs::remove_file(&self.path) {
            Ok(()) => {
                tracing::info!(target: "ternvale::cli", path = %self.path.display(), "control socket removed")
            }
            Err(error) => {
                tracing::warn!(target: "ternvale::cli", path = %self.path.display(), error = %error, "could not remove control socket")
            }
        }
    }
}

/// Remove `path` if nothing answers on it; fail if a live VM does.
fn clear_stale(path: &Path) -> Result<()> {
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    if UnixStream::connect(path).is_ok() {
        bail!(
            "a vm is already serving {} (stop it first, or use another name)",
            path.display()
        );
    }
    tracing::warn!(target: "ternvale::cli", path = %path.display(), "removing stale control socket");
    std::fs::remove_file(path)
        .with_context(|| format!("remove stale control socket {}", path.display()))
}

fn accept_loop(listener: &UnixListener, control: &Arc<VmControl>, stop: &AtomicBool) {
    let mut next_id = 0u64;
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                next_id += 1;
                let id = next_id;
                let control = Arc::clone(control);
                let spawned = std::thread::Builder::new()
                    .name(format!("control-{id}"))
                    .spawn(move || serve(stream, &control, id));
                if let Err(error) = spawned {
                    tracing::error!(target: "ternvale::cli", conn = id, error = %error, "could not spawn control connection thread");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(error) => {
                tracing::warn!(target: "ternvale::cli", error = %error, "control socket accept failed");
                std::thread::sleep(ACCEPT_POLL);
            }
        }
    }
    tracing::debug!(target: "ternvale::cli", "control socket accept loop stopped");
}

fn serve(stream: UnixStream, control: &VmControl, id: u64) {
    let span =
        tracing::info_span!(target: "ternvale::cli", "control", vm = control.name(), conn = id);
    let _entered = span.enter();
    if let Err(error) = serve_lines(stream, control) {
        tracing::warn!(target: "ternvale::cli", error = %format!("{error:#}"), "control connection ended with an error");
    }
}

fn serve_lines(stream: UnixStream, control: &VmControl) -> Result<()> {
    stream
        .set_nonblocking(false)
        .context("make control connection blocking")?;
    stream
        .set_read_timeout(Some(IDLE_TIMEOUT))
        .context("set control connection read timeout")?;
    let mut writer = stream.try_clone().context("clone control connection")?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        let read = reader
            .by_ref()
            .take(MAX_LINE as u64 + 1)
            .read_line(&mut line)
            .context("read control request")?;
        if read == 0 {
            tracing::debug!(target: "ternvale::cli", "control connection closed");
            return Ok(());
        }
        let response = if line.len() > MAX_LINE {
            tracing::warn!(target: "ternvale::cli", bytes = line.len(), "control request too long");
            Response::error(format!("request longer than {MAX_LINE} bytes"))
        } else {
            handle_line(line.trim_end(), control)
        };
        let mut out = serde_json::to_string(&response).context("encode control response")?;
        out.push('\n');
        writer
            .write_all(out.as_bytes())
            .context("write control response")?;
        if line.len() > MAX_LINE {
            return Ok(());
        }
    }
}

/// Parse one request line and apply it to `control`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = control.name()))]
pub fn handle_line(line: &str, control: &VmControl) -> Response {
    let request: Request = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            tracing::warn!(target: "ternvale::cli", error = %error, "malformed control request");
            return Response::error(format!("malformed request: {error}"));
        }
    };
    tracing::info!(target: "ternvale::cli", cmd = request.name(), "control request");
    let result = match request {
        Request::Status => Ok(Response::status(&control.status())),
        Request::QueryStats => Ok(Response::stats(&control.stats())),
        Request::Pause { timeout_ms } => {
            let wait = timeout_ms.unwrap_or(DEFAULT_PAUSE_MS).min(MAX_PAUSE_MS);
            control
                .pause(Duration::from_millis(wait))
                .map(|status| Response::status(&status))
        }
        Request::Resume => control.resume().map(|status| Response::status(&status)),
        Request::Shutdown => control
            .request_stop(false)
            .map(|status| Response::status(&status)),
        Request::ForceStop => control
            .request_stop(true)
            .map(|status| Response::status(&status)),
    };
    match result {
        Ok(response) => {
            tracing::debug!(target: "ternvale::cli", state = ?response.status.as_ref().map(|s| s.state.as_str()), "control request done");
            response
        }
        Err(error) => {
            tracing::warn!(target: "ternvale::cli", error = %error, "control request failed");
            Response::error(error.to_string())
        }
    }
}
