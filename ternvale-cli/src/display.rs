//! `ternvale run --display window`: virtio-gpu frames on a Unix socket.
//!
//! By default the CLI also launches the Step 44 dev viewer (`tools/ternvale-viewer`).
//! The macOS app sets [`EMBEDDED_DISPLAY_ENV`] and connects its own Metal view instead.
//!
//! The run adds a virtio-gpu (next virtio-mmio slot, or PCI with
//! `profile = "windows"`), starts a [`Presenter`] on
//! `<run dir>/<name>.display.sock` capped at `--fps`, and optionally launches the
//! viewer. A virtio-input keyboard and tablet follow the GPU and take the
//! window's input ([`crate::display_input`]). Closing the viewer leaves the VM
//! running; stopping the VM closes the socket.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use ternvale_config::VirtioTransport;
use ternvale_gpu::{GpuConfig, GpuDisplay, InputTarget, PresentConfig, Presenter, VirtioGpu};
use ternvale_vmm::{DeviceAttach, MachineError, MmioDevice};

use crate::paths::{MAX_SOCKET_PATH, RUN_DIR_ENV};

/// Viewer binary override.
pub const VIEWER_ENV: &str = "TERNVALE_VIEWER";
/// When set, the presenter listens but no dev viewer process is spawned (app Metal view).
pub const EMBEDDED_DISPLAY_ENV: &str = "TERNVALE_EMBEDDED_DISPLAY";
/// The viewer's file name next to `ternvale` or under `target/viewer`.
pub const VIEWER_NAME: &str = "ternvale-viewer";
/// How long the viewer gets to exit after its socket closes.
const VIEWER_EXIT_GRACE: Duration = Duration::from_secs(2);

/// Host display for the VM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum DisplayMode {
    /// No virtio-gpu.
    #[default]
    None,
    /// A virtio-gpu with a presenter socket (dev viewer or app).
    Window,
}

impl std::fmt::Display for DisplayMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Window => "window",
        })
    }
}

/// Where the viewer may live relative to the `ternvale` binary's directory:
/// beside it (installed), or `target/viewer` (built by `make viewer`).
pub fn viewer_candidates(exe_dir: &Path) -> [PathBuf; 2] {
    [
        exe_dir.join(VIEWER_NAME),
        exe_dir.join("..").join("viewer").join(VIEWER_NAME),
    ]
}

/// The viewer binary: `$TERNVALE_VIEWER`, else the first of [`viewer_candidates`].
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn find_viewer() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("find the ternvale binary")?;
    locate_viewer(
        std::env::var_os(VIEWER_ENV).map(PathBuf::from),
        exe.parent().unwrap_or(Path::new(".")),
    )
}

/// [`find_viewer`] with the override and the binary's directory given.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(exe_dir = %exe_dir.display()))]
pub fn locate_viewer(overridden: Option<PathBuf>, exe_dir: &Path) -> Result<PathBuf> {
    if let Some(path) = overridden {
        if !path.is_file() {
            tracing::warn!(target: "ternvale::cli", viewer = %path.display(), "{VIEWER_ENV} is not a file");
            bail!("{VIEWER_ENV}={} is not a file", path.display());
        }
        tracing::debug!(target: "ternvale::cli", viewer = %path.display(), "viewer from {VIEWER_ENV}");
        return Ok(path);
    }
    let candidates = viewer_candidates(exe_dir);
    if let Some(found) = candidates.iter().find(|path| path.is_file()) {
        tracing::debug!(target: "ternvale::cli", viewer = %found.display(), "found dev viewer");
        return Ok(found.clone());
    }
    tracing::warn!(target: "ternvale::cli", looked = ?candidates, "dev viewer not found");
    bail!(
        "dev viewer {VIEWER_NAME} not found (looked for {} and {}); build it with `make viewer` or set {VIEWER_ENV}",
        candidates[0].display(),
        candidates[1].display()
    )
}

/// The display socket beside the control socket: `<name>.display.sock`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(control = %control.display()))]
pub fn socket_path(control: &Path) -> Result<PathBuf> {
    let path = control.with_extension("display.sock");
    let len = path.as_os_str().len();
    if len > MAX_SOCKET_PATH {
        tracing::warn!(target: "ternvale::cli", path = %path.display(), len, "display socket path too long");
        bail!(
            "display socket path {} is {len} bytes; macOS allows {MAX_SOCKET_PATH} (set {RUN_DIR_ENV} to a shorter directory)",
            path.display()
        );
    }
    Ok(path)
}

/// What `--display window` needs, resolved before the VM boots so a missing
/// viewer fails fast (unless embedded display is requested).
#[derive(Debug, Clone)]
pub struct DisplayPlan {
    /// VM name, for the window title.
    pub name: String,
    /// Dev viewer binary, when the app is not presenting.
    pub viewer: Option<PathBuf>,
    /// Display socket.
    pub socket: PathBuf,
    /// Frame-rate cap.
    pub fps: u32,
}

impl DisplayPlan {
    /// Resolve the socket for VM `name` with control socket `control`.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name, fps))]
    pub fn new(name: &str, control: &Path, fps: u32) -> Result<Self> {
        let embedded = std::env::var_os(EMBEDDED_DISPLAY_ENV).is_some();
        let viewer = if embedded {
            tracing::info!(target: "ternvale::cli", "embedded display; dev viewer not launched");
            None
        } else {
            Some(find_viewer()?)
        };
        Ok(Self {
            name: name.to_string(),
            viewer,
            socket: socket_path(control)?,
            fps,
        })
    }

    /// Start presenting `display`, forwarding window input to `input`, and
    /// launch the dev viewer when configured.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = %self.name, input = input.is_some()))]
    pub fn start(
        &self,
        display: GpuDisplay,
        input: Option<Arc<dyn InputTarget>>,
    ) -> Result<DisplaySession> {
        let config = PresentConfig {
            fps: self.fps,
            ..PresentConfig::default()
        };
        let presenter = Presenter::start_with_input(display, &self.socket, config, input)
            .with_context(|| format!("start the display presenter on {}", self.socket.display()))?;
        let viewer = match &self.viewer {
            Some(viewer) => {
                let child = Command::new(viewer)
                    .arg("--socket")
                    .arg(&self.socket)
                    .arg("--title")
                    .arg(format!("Ternvale — {}", self.name))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .with_context(|| format!("launch the dev viewer {}", viewer.display()))?;
                tracing::info!(target: "ternvale::cli", viewer = %viewer.display(), pid = child.id(),
                    socket = %self.socket.display(), fps = self.fps, "started display viewer");
                Some(child)
            }
            None => {
                tracing::info!(target: "ternvale::cli", socket = %self.socket.display(),
                    fps = self.fps, "display presenter ready for embedded client");
                None
            }
        };
        Ok(DisplaySession {
            presenter: Some(presenter),
            viewer,
        })
    }
}

/// A presenter and optionally its dev viewer process; dropping it closes both.
pub struct DisplaySession {
    presenter: Option<Presenter>,
    viewer: Option<Child>,
}

impl Drop for DisplaySession {
    fn drop(&mut self) {
        drop(self.presenter.take());
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        let deadline = Instant::now() + VIEWER_EXIT_GRACE;
        loop {
            match viewer.try_wait() {
                Ok(Some(status)) => {
                    tracing::info!(target: "ternvale::cli", %status, "display viewer exited");
                    return;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(target: "ternvale::cli", error = %error, "could not poll the display viewer");
                    break;
                }
            }
        }
        tracing::warn!(target: "ternvale::cli", pid = viewer.id(), "display viewer did not exit; killing it");
        if let Err(error) = viewer.kill() {
            tracing::warn!(target: "ternvale::cli", error = %error, "could not kill the display viewer");
        }
        if let Err(error) = viewer.wait() {
            tracing::warn!(target: "ternvale::cli", error = %error, "could not reap the display viewer");
        }
    }
}

/// An MMIO device to map: base, size, device.
pub type Mapped = (u64, u64, Box<dyn MmioDevice>);

/// Add a virtio-gpu: on `slot` for virtio-mmio, else on the next PCI function.
/// Returns the MMIO device to map (none for PCI) and the frame handle.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(slot, transport = %transport))]
pub fn attach_gpu(
    attach: &DeviceAttach,
    transport: VirtioTransport,
    slot: u32,
) -> Result<(Option<Mapped>, GpuDisplay), MachineError> {
    if transport == VirtioTransport::Pci {
        let gpu = VirtioGpu::attach_pci(attach, GpuConfig::default())?;
        tracing::info!(target: "ternvale::cli", bdf = %gpu.bdf, "attached virtio-gpu");
        return Ok((None, gpu.display));
    }
    let gpu = VirtioGpu::attach(
        slot,
        GpuConfig::default(),
        Arc::clone(&attach.memory),
        attach.virtio_irq_hook(slot),
    )
    .map_err(|error| MachineError::Attach(format!("virtio-gpu: {error}")))?;
    tracing::info!(target: "ternvale::cli", slot, base = %format!("{:#x}", gpu.base), "attached virtio-gpu");
    Ok((Some((gpu.base, gpu.size, gpu.device)), gpu.display))
}

#[cfg(test)]
#[path = "display_tests.rs"]
mod tests;
