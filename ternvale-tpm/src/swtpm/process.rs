//! Spawning swtpm and stopping it.
//!
//! Each run gets a private runtime directory (mode 0700) under the temp dir
//! holding the two sockets swtpm listens on. swtpm's stdout and stderr are
//! forwarded line by line to `ternvale::tpm`. `--ctrl …,terminate` makes
//! swtpm exit when the control connection drops, so it does not outlive a
//! VMM that crashed.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::SwtpmError;

/// macOS `sun_path` is 104 bytes including the terminating NUL.
const MAX_SOCKET_PATH: usize = 103;
/// How long swtpm gets to create its sockets.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long swtpm gets to exit after `CMD_SHUTDOWN` before it is killed.
const EXIT_TIMEOUT: Duration = Duration::from_secs(3);
/// swtpm output lines kept for error messages.
const KEPT_LINES: usize = 20;

static NEXT_RUN: AtomicU64 = AtomicU64::new(0);

type Lines = Arc<Mutex<VecDeque<String>>>;

/// The argument list after the binary. `docs/TPM.md` explains every flag.
pub(super) fn swtpm_args(state: &str, data: &str, ctrl: &str) -> Vec<String> {
    vec![
        "socket".into(),
        "--tpm2".into(),
        "--tpmstate".into(),
        format!("backend-uri=file://{state},lock"),
        "--server".into(),
        format!("type=unixio,path={data}"),
        "--ctrl".into(),
        format!("type=unixio,path={ctrl},terminate"),
    ]
}

/// `path` as option text: UTF-8, absolute, no `,` (swtpm splits option
/// parameters on commas and has no escape for them).
pub(super) fn option_path(what: &'static str, path: &Path) -> Result<String, SwtpmError> {
    let reject = |reason: &'static str| {
        tracing::error!(target: "ternvale::tpm", what, path = %path.display(), reason, "path cannot be passed to swtpm");
        SwtpmError::Path {
            what,
            path: path.to_path_buf(),
            reason,
        }
    };
    let absolute = std::path::absolute(path).map_err(|_| reject("cannot make it absolute"))?;
    let text = absolute.to_str().ok_or_else(|| reject("not UTF-8"))?;
    if text.contains(',') {
        return Err(reject("contains ','"));
    }
    Ok(text.to_string())
}

/// A running swtpm.
#[derive(Debug)]
pub(super) struct SwtpmProcess {
    child: Child,
    runtime_dir: PathBuf,
    lines: Lines,
    readers: Vec<JoinHandle<()>>,
    /// `CMD_SHUTDOWN` succeeded, so swtpm is exiting on its own.
    exiting: bool,
    stopped: bool,
}

impl SwtpmProcess {
    /// Start `binary` on `state` and connect to its control and data sockets.
    pub(super) fn spawn(
        binary: &Path,
        state: &Path,
    ) -> Result<(Self, UnixStream, UnixStream), SwtpmError> {
        let spawn_error = |reason: String| {
            tracing::error!(target: "ternvale::tpm", reason = %reason, "swtpm did not start");
            SwtpmError::Spawn { reason }
        };
        let state_text = option_path("tpm state file", state)?;
        let runtime_dir = std::env::temp_dir().join(format!(
            "ternvale-swtpm-{}-{}",
            std::process::id(),
            NEXT_RUN.fetch_add(1, Ordering::Relaxed)
        ));
        let (data_path, ctrl_path) = (runtime_dir.join("data.sock"), runtime_dir.join("ctrl.sock"));
        let data_text = option_path("swtpm data socket", &data_path)?;
        let ctrl_text = option_path("swtpm control socket", &ctrl_path)?;
        if data_text.len().max(ctrl_text.len()) > MAX_SOCKET_PATH {
            return Err(spawn_error(format!(
                "socket path {ctrl_text} is longer than {MAX_SOCKET_PATH} bytes; set TMPDIR to a shorter directory"
            )));
        }
        make_private_dir(&runtime_dir).map_err(|error| {
            spawn_error(format!("runtime dir {}: {error}", runtime_dir.display()))
        })?;
        let args = swtpm_args(&state_text, &data_text, &ctrl_text);
        tracing::info!(target: "ternvale::tpm", binary = %binary.display(), args = %args.join(" "), "starting swtpm");
        let mut child = match Command::new(binary)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                remove_dir(&runtime_dir);
                return Err(spawn_error(format!("{}: {error}", binary.display())));
            }
        };
        let lines: Lines = Arc::default();
        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            readers.push(forward(out, "stdout", Arc::clone(&lines)));
        }
        if let Some(err) = child.stderr.take() {
            readers.push(forward(err, "stderr", Arc::clone(&lines)));
        }
        let mut process = Self {
            child,
            runtime_dir,
            lines,
            readers,
            exiting: false,
            stopped: false,
        };
        let ctrl = process.connect(&ctrl_path)?;
        let data = process.connect(&data_path)?;
        tracing::debug!(target: "ternvale::tpm", pid = process.child.id(), runtime_dir = %process.runtime_dir.display(), "connected to swtpm");
        Ok((process, ctrl, data))
    }

    /// `CMD_SHUTDOWN` succeeded: give swtpm time to exit before killing it.
    pub(super) fn exiting(&mut self) {
        self.exiting = true;
    }

    /// Process id, for logs.
    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The last lines swtpm printed, for error messages.
    pub(super) fn recent_output(&self) -> String {
        let lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        if lines.is_empty() {
            "no output".into()
        } else {
            lines.iter().cloned().collect::<Vec<_>>().join(" | ")
        }
    }

    /// Connect to `path`, retrying until swtpm has created it or exited.
    fn connect(&mut self, path: &Path) -> Result<UnixStream, SwtpmError> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            match UnixStream::connect(path) {
                Ok(stream) => return Ok(stream),
                Err(error) => {
                    if let Ok(Some(status)) = self.child.try_wait() {
                        let reason = format!(
                            "swtpm exited with {status} before {} appeared: {}",
                            path.display(),
                            self.output_after_exit()
                        );
                        tracing::error!(target: "ternvale::tpm", reason = %reason, "swtpm did not start");
                        return Err(SwtpmError::Spawn { reason });
                    }
                    if Instant::now() >= deadline {
                        let reason = format!(
                            "{} did not accept within {CONNECT_TIMEOUT:?}: {error}",
                            path.display()
                        );
                        tracing::error!(target: "ternvale::tpm", reason = %reason, "swtpm did not start");
                        return Err(SwtpmError::Spawn { reason });
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    /// [`Self::recent_output`] once the forwarders have drained the pipes.
    fn output_after_exit(&mut self) -> String {
        for reader in self.readers.drain(..) {
            if reader.join().is_err() {
                tracing::warn!(target: "ternvale::tpm", "swtpm output forwarder panicked");
            }
        }
        self.recent_output()
    }
}

impl SwtpmProcess {
    /// Stop swtpm now and return everything it printed, for start-up errors.
    pub(super) fn stop(mut self) -> String {
        self.halt()
    }

    fn halt(&mut self) -> String {
        if self.stopped {
            return self.recent_output();
        }
        self.stopped = true;
        let deadline = Instant::now()
            + if self.exiting {
                EXIT_TIMEOUT
            } else {
                Duration::ZERO
            };
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Ok(None) => {
                    if self.exiting {
                        tracing::warn!(target: "ternvale::tpm", pid = self.child.id(), "swtpm did not exit after CMD_SHUTDOWN; killing it");
                    } else {
                        tracing::debug!(target: "ternvale::tpm", pid = self.child.id(), "killing swtpm (no CMD_SHUTDOWN)");
                    }
                    if let Err(error) = self.child.kill() {
                        tracing::error!(target: "ternvale::tpm", error = %error, "kill swtpm failed");
                    }
                    break self.child.wait().ok();
                }
                Err(error) => {
                    tracing::error!(target: "ternvale::tpm", error = %error, "wait for swtpm failed");
                    break None;
                }
            }
        };
        let output = self.output_after_exit();
        tracing::info!(target: "ternvale::tpm", pid = self.child.id(), status = ?status, "swtpm stopped");
        tracing::debug!(target: "ternvale::tpm", output = %output, "swtpm last output");
        remove_dir(&self.runtime_dir);
        output
    }
}

impl Drop for SwtpmProcess {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Log each line of `pipe` and keep the last [`KEPT_LINES`].
fn forward(pipe: impl Read + Send + 'static, stream: &'static str, lines: Lines) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            let Ok(line) = line else { break };
            tracing::info!(target: "ternvale::tpm", stream, line = %line, "swtpm output");
            let mut kept = lines.lock().unwrap_or_else(PoisonError::into_inner);
            if kept.len() == KEPT_LINES {
                kept.pop_front();
            }
            kept.push_back(line);
        }
    })
}

fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

fn remove_dir(dir: &Path) {
    if let Err(error) = std::fs::remove_dir_all(dir) {
        tracing::warn!(target: "ternvale::tpm", dir = %dir.display(), error = %error, "could not remove the swtpm runtime directory");
    }
}
