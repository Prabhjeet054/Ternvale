//! Host presentation (Step 44): stream one scanout's frames to the dev viewer
//! over a Unix socket, capped at a configurable frame rate.
//!
//! The presenter owns a thread (`ternvale-gpu-present`) that listens on the
//! socket and serves one viewer at a time (a closed viewer may reconnect).
//! It only ever reads [`GpuDisplay`] copies, so the vCPU and the control-queue
//! worker never wait on the viewer. Per-frame logs (DEBUG, `ternvale::gpu`):
//! `frames dropped` (count, reason) and `frame presented`
//! (`flush_to_present_us` from the guest's `RESOURCE_FLUSH` to the viewer's
//! on-screen ack, `send_to_present_us` for the socket + draw leg alone).

use std::io::ErrorKind;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::present_protocol::ViewerInput;
use crate::present_session;
use crate::scanout::GpuDisplay;

/// Default `--fps`.
pub const DEFAULT_FPS: u32 = 30;
/// Highest accepted `--fps`.
pub const MAX_FPS: u32 = 240;
/// Default [`PresentConfig::report_every`].
pub const DEFAULT_REPORT_EVERY: Duration = Duration::from_secs(10);
/// How often the idle listener checks for a stop request.
const ACCEPT_POLL: Duration = Duration::from_millis(20);

/// What to present and how fast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentConfig {
    /// Frames per second, at most (`1..=MAX_FPS`).
    pub fps: u32,
    /// Which scanout to show.
    pub scanout: usize,
    /// How often a session logs its `display present window` statistics.
    pub report_every: Duration,
}

impl Default for PresentConfig {
    fn default() -> Self {
        Self {
            fps: DEFAULT_FPS,
            scanout: 0,
            report_every: DEFAULT_REPORT_EVERY,
        }
    }
}

impl PresentConfig {
    /// Time between frames.
    pub fn interval(&self) -> Duration {
        Duration::from_secs(1) / self.fps.max(1)
    }
}

/// Why the presenter could not start.
#[derive(Debug, thiserror::Error)]
pub enum PresentError {
    /// `fps` or `scanout` out of range.
    #[error("invalid present config: {0}")]
    Config(String),
    /// The socket could not be created.
    #[error("{op} display socket {path}: {source}")]
    Socket {
        /// What failed.
        op: &'static str,
        /// The socket path.
        path: PathBuf,
        /// The OS error.
        source: std::io::Error,
    },
    /// The thread could not start.
    #[error("spawn presenter thread: {0}")]
    Spawn(std::io::Error),
}

/// Running totals, readable while the presenter runs.
#[derive(Debug, Default)]
pub struct PresentStats {
    /// Viewers that connected and said a valid hello.
    pub viewers: AtomicU64,
    /// Frames written to a viewer.
    pub sent: AtomicU64,
    /// Frames the viewer acknowledged as on screen.
    pub presented: AtomicU64,
    /// Guest frame updates never sent (FPS cap or busy viewer).
    pub dropped: AtomicU64,
    /// Sum of flush-to-present latencies, µs.
    pub latency_sum_us: AtomicU64,
    /// Largest flush-to-present latency, µs.
    pub latency_max_us: AtomicU64,
    /// Frames counted in `latency_sum_us`.
    pub latency_samples: AtomicU64,
    /// Input messages received from viewers.
    pub inputs: AtomicU64,
}

/// Where viewer input goes (the VM's virtio-input devices). Called on the
/// presenter's socket reader thread as each message arrives, so it must not
/// block for long.
pub trait InputTarget: Send + Sync {
    /// Forward one input message.
    fn deliver(&self, input: ViewerInput);
}

impl PresentStats {
    /// Mean flush-to-present latency in µs, if any frame was measured.
    pub fn average_latency_us(&self) -> Option<u64> {
        let samples = self.latency_samples.load(Ordering::Relaxed);
        (samples > 0).then(|| self.latency_sum_us.load(Ordering::Relaxed) / samples)
    }

    pub(crate) fn record_latency(&self, micros: u64) {
        self.latency_sum_us.fetch_add(micros, Ordering::Relaxed);
        self.latency_samples.fetch_add(1, Ordering::Relaxed);
        self.latency_max_us.fetch_max(micros, Ordering::Relaxed);
    }
}

/// A running presenter; dropping it stops the thread and removes the socket.
pub struct Presenter {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    stats: Arc<PresentStats>,
    thread: Option<JoinHandle<()>>,
    started: Instant,
}

impl Presenter {
    /// Listen on `socket` (replacing a stale file) and start serving viewers.
    /// Viewer input is discarded.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all,
        fields(socket = %socket.display(), fps = config.fps, scanout = config.scanout))]
    pub fn start(
        display: GpuDisplay,
        socket: &Path,
        config: PresentConfig,
    ) -> Result<Self, PresentError> {
        Self::start_with_input(display, socket, config, None)
    }

    /// [`Presenter::start`], forwarding viewer input to `input`.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all,
        fields(socket = %socket.display(), fps = config.fps, input = input.is_some()))]
    pub fn start_with_input(
        display: GpuDisplay,
        socket: &Path,
        config: PresentConfig,
        input: Option<Arc<dyn InputTarget>>,
    ) -> Result<Self, PresentError> {
        if config.fps == 0 || config.fps > MAX_FPS {
            return Err(PresentError::Config(format!(
                "fps {} outside 1..={MAX_FPS}",
                config.fps
            )));
        }
        if config.report_every.is_zero() {
            return Err(PresentError::Config("report_every must be non-zero".into()));
        }
        if config.scanout >= display.scanouts() {
            return Err(PresentError::Config(format!(
                "scanout {} but the device has {}",
                config.scanout,
                display.scanouts()
            )));
        }
        let socket_err = |op| {
            let path = socket.to_path_buf();
            move |source| PresentError::Socket { op, path, source }
        };
        match std::fs::remove_file(socket) {
            Ok(()) => tracing::debug!(target: "ternvale::gpu", "removed stale display socket"),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(socket_err("remove stale")(e)),
        }
        let listener = UnixListener::bind(socket).map_err(socket_err("bind"))?;
        listener
            .set_nonblocking(true)
            .map_err(socket_err("set non-blocking on"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(PresentStats::default());
        let thread = {
            let (stop, stats) = (stop.clone(), stats.clone());
            std::thread::Builder::new()
                .name("ternvale-gpu-present".into())
                .spawn(move || {
                    let serving = Serving {
                        display: &display,
                        config,
                        stats: &stats,
                        stop: &stop,
                        input: input.as_deref(),
                    };
                    listen(&listener, &serving)
                })
                .map_err(PresentError::Spawn)?
        };
        tracing::info!(target: "ternvale::gpu", socket = %socket.display(), fps = config.fps,
            "display presenter listening");
        Ok(Self {
            socket: socket.to_path_buf(),
            stop,
            stats,
            thread: Some(thread),
            started: Instant::now(),
        })
    }

    /// The socket viewers connect to.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Live totals.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn stats(&self) -> Arc<PresentStats> {
        self.stats.clone()
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::error!(target: "ternvale::gpu", "display presenter thread panicked");
            }
        }
        if let Err(e) = std::fs::remove_file(&self.socket) {
            if e.kind() != ErrorKind::NotFound {
                tracing::warn!(target: "ternvale::gpu", socket = %self.socket.display(),
                    error = %e, "could not remove display socket");
            }
        }
        let s = &self.stats;
        tracing::info!(target: "ternvale::gpu",
            uptime_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            viewers = s.viewers.load(Ordering::Relaxed),
            sent = s.sent.load(Ordering::Relaxed),
            presented = s.presented.load(Ordering::Relaxed),
            dropped = s.dropped.load(Ordering::Relaxed),
            latency_samples = s.latency_samples.load(Ordering::Relaxed),
            avg_flush_to_present_us = s.average_latency_us().unwrap_or(0),
            max_flush_to_present_us = s.latency_max_us.load(Ordering::Relaxed),
            inputs = s.inputs.load(Ordering::Relaxed),
            "display presenter stopped");
    }
}

/// What a viewer session works with.
pub(crate) struct Serving<'a> {
    pub(crate) display: &'a GpuDisplay,
    pub(crate) config: PresentConfig,
    pub(crate) stats: &'a PresentStats,
    pub(crate) stop: &'a AtomicBool,
    pub(crate) input: Option<&'a dyn InputTarget>,
}

/// Accept viewers one after another until stopped.
fn listen(listener: &UnixListener, serving: &Serving<'_>) {
    while !serving.stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                tracing::info!(target: "ternvale::gpu", "display viewer connected");
                match present_session::serve(stream, serving) {
                    Ok(why) => {
                        tracing::info!(target: "ternvale::gpu", reason = why, "display viewer disconnected")
                    }
                    Err(e) => {
                        tracing::warn!(target: "ternvale::gpu", error = %e, "display viewer session failed")
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(ACCEPT_POLL),
            Err(e) => {
                tracing::warn!(target: "ternvale::gpu", error = %e, "accept on display socket failed");
                std::thread::sleep(ACCEPT_POLL);
            }
        }
    }
}

#[cfg(test)]
#[path = "present_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "present_input_tests.rs"]
mod input_tests;

#[cfg(test)]
#[path = "present_viewer_tests.rs"]
mod viewer_tests;
