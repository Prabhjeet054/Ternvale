//! `ternvale run --display window`: show the VM's virtio-gpu in the dev
//! viewer (`tools/ternvale-viewer`, Step 44; a throwaway tool, not the app).
//!
//! The run adds a virtio-gpu (next virtio-mmio slot, or PCI with
//! `profile = "windows"`), starts a [`Presenter`] on
//! `<run dir>/<name>.display.sock` capped at `--fps`, and launches the viewer
//! pointed at it. Closing the window leaves the VM running; stopping the VM
//! closes the socket and the viewer exits.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use ternvale_config::VirtioTransport;
use ternvale_gpu::{GpuConfig, GpuDisplay, PresentConfig, Presenter, VirtioGpu};
use ternvale_vmm::{DeviceAttach, MachineError, MmioDevice};

use crate::paths::{MAX_SOCKET_PATH, RUN_DIR_ENV};

/// Viewer binary override.
pub const VIEWER_ENV: &str = "TERNVALE_VIEWER";
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
    /// A virtio-gpu shown in the dev viewer window.
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
/// viewer fails fast.
#[derive(Debug, Clone)]
pub struct DisplayPlan {
    /// VM name, for the window title.
    pub name: String,
    /// Viewer binary.
    pub viewer: PathBuf,
    /// Display socket.
    pub socket: PathBuf,
    /// Frame-rate cap.
    pub fps: u32,
}

impl DisplayPlan {
    /// Resolve the viewer and socket for VM `name` with control socket `control`.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name, fps))]
    pub fn new(name: &str, control: &Path, fps: u32) -> Result<Self> {
        Ok(Self {
            name: name.to_string(),
            viewer: find_viewer()?,
            socket: socket_path(control)?,
            fps,
        })
    }

    /// Start presenting `display` and launch the viewer.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = %self.name))]
    pub fn start(&self, display: GpuDisplay) -> Result<DisplaySession> {
        let config = PresentConfig {
            fps: self.fps,
            ..PresentConfig::default()
        };
        let presenter = Presenter::start(display, &self.socket, config)
            .with_context(|| format!("start the display presenter on {}", self.socket.display()))?;
        let viewer = Command::new(&self.viewer)
            .arg("--socket")
            .arg(&self.socket)
            .arg("--title")
            .arg(format!("Ternvale — {}", self.name))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("launch the dev viewer {}", self.viewer.display()))?;
        tracing::info!(target: "ternvale::cli", viewer = %self.viewer.display(), pid = viewer.id(),
            socket = %self.socket.display(), fps = self.fps, "started display viewer");
        Ok(DisplaySession {
            presenter: Some(presenter),
            viewer,
        })
    }
}

/// A presenter and its viewer process; dropping it closes both.
pub struct DisplaySession {
    presenter: Option<Presenter>,
    viewer: Child,
}

impl Drop for DisplaySession {
    fn drop(&mut self) {
        drop(self.presenter.take());
        let deadline = Instant::now() + VIEWER_EXIT_GRACE;
        loop {
            match self.viewer.try_wait() {
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
        tracing::warn!(target: "ternvale::cli", pid = self.viewer.id(), "display viewer did not exit; killing it");
        if let Err(error) = self.viewer.kill() {
            tracing::warn!(target: "ternvale::cli", error = %error, "could not kill the display viewer");
        }
        if let Err(error) = self.viewer.wait() {
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
        std::sync::Arc::clone(&attach.memory),
        attach.virtio_irq_hook(slot),
    )
    .map_err(|error| MachineError::Attach(format!("virtio-gpu: {error}")))?;
    tracing::info!(target: "ternvale::cli", slot, base = %format!("{:#x}", gpu.base), "attached virtio-gpu");
    Ok((Some((gpu.base, gpu.size, gpu.device)), gpu.display))
}

#[cfg(test)]
#[path = "display_tests.rs"]
mod tests;
