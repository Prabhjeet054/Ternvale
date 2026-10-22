//! Host-side pixel buffers, one per scanout, and the handle that reads them.
//!
//! The control-queue worker writes a scanout's [`Frame`] on `SET_SCANOUT` and
//! `RESOURCE_FLUSH`; anything holding a [`GpuDisplay`] (the presenter, tests)
//! reads a copy. `sequence` increases on every update, so a reader can
//! tell whether a frame is new without comparing pixels.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::gpu2d::MAX_DIMENSION;
use crate::protocol::Rect;
use crate::resource::{PixelFormat, Resource, BYTES_PER_PIXEL};

/// Host-requested mode change was invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostModeError {
    /// Scanout index out of range.
    #[error("scanout {scanout} out of range (device has {scanouts} scanouts)")]
    Scanout {
        /// Requested scanout.
        scanout: usize,
        /// Scanouts on the device.
        scanouts: usize,
    },
    /// Width or height out of range.
    #[error("mode {width}x{height} outside 1..={max}")]
    Dimensions {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
        /// Largest allowed dimension.
        max: u32,
    },
}

type HostResizeHook = Box<dyn Fn(usize, u32, u32) + Send + Sync>;

struct Inner {
    modes: Mutex<Vec<(u32, u32)>>,
    frames: Vec<Mutex<Frame>>,
    on_host_resize: Mutex<Option<HostResizeHook>>,
}

/// What one scanout shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    /// Width in pixels (0 while disabled).
    pub width: u32,
    /// Height in pixels (0 while disabled).
    pub height: u32,
    /// Layout of `pixels`; `None` while the scanout is disabled.
    pub format: Option<PixelFormat>,
    /// `height` rows of `width × 4` bytes.
    pub pixels: Vec<u8>,
    /// Bumped on every `SET_SCANOUT` and on every flush that touched this scanout.
    pub sequence: u64,
    /// When the `RESOURCE_FLUSH` (or `SET_SCANOUT`) behind `sequence` reached
    /// the device; the presenter measures flush-to-present latency from here.
    pub updated: Option<Instant>,
}

impl Frame {
    /// Bytes per row.
    pub fn stride(&self) -> usize {
        self.width as usize * BYTES_PER_PIXEL as usize
    }

    /// The four bytes of pixel (`x`, `y`), if inside the frame.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = y as usize * self.stride() + x as usize * BYTES_PER_PIXEL as usize;
        let mut out = [0u8; 4];
        out.copy_from_slice(self.pixels.get(at..at + 4)?);
        Some(out)
    }
}

/// Cloneable read handle on every scanout's [`Frame`] and its advertised mode.
#[derive(Clone)]
pub struct GpuDisplay {
    inner: Arc<Inner>,
}

impl GpuDisplay {
    /// One disabled frame per mode (width, height) the device advertises.
    pub(crate) fn new(modes: &[(u32, u32)]) -> Self {
        Self {
            inner: Arc::new(Inner {
                modes: Mutex::new(modes.to_vec()),
                frames: modes.iter().map(|_| Mutex::new(Frame::default())).collect(),
                on_host_resize: Mutex::new(None),
            }),
        }
    }

    /// Called when the viewer asks for a new mode (virtio `GET_DISPLAY_INFO`).
    pub(crate) fn install_host_resize_hook(&self, hook: HostResizeHook) {
        let mut slot =
            ternvale_vmm::lockwatch::lock(&self.inner.on_host_resize, "gpu-host-resize-hook");
        *slot = Some(hook);
    }

    /// Update the advertised mode for `scanout` and notify the virtio-gpu device.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu",
        skip(self),
        fields(scanout, width, height)
    )]
    pub fn request_host_mode(
        &self,
        scanout: usize,
        width: u32,
        height: u32,
    ) -> Result<(), HostModeError> {
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(HostModeError::Dimensions {
                width,
                height,
                max: MAX_DIMENSION,
            });
        }
        let mut modes = ternvale_vmm::lockwatch::lock(&self.inner.modes, "gpu-display-modes");
        if scanout >= modes.len() {
            return Err(HostModeError::Scanout {
                scanout,
                scanouts: modes.len(),
            });
        }
        if modes[scanout] == (width, height) {
            tracing::trace!(target: "ternvale::gpu", scanout, width, height, "host mode unchanged");
            return Ok(());
        }
        modes[scanout] = (width, height);
        tracing::info!(target: "ternvale::gpu", scanout, width, height, "host display mode updated");
        let hook =
            ternvale_vmm::lockwatch::lock(&self.inner.on_host_resize, "gpu-host-resize-hook");
        if let Some(notify) = hook.as_ref() {
            notify(scanout, width, height);
        }
        Ok(())
    }

    /// Number of scanouts.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn scanouts(&self) -> usize {
        self.inner.frames.len()
    }

    /// The mode `GET_DISPLAY_INFO` reports for `scanout`.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all, fields(scanout))]
    pub fn mode(&self, scanout: usize) -> Option<(u32, u32)> {
        ternvale_vmm::lockwatch::lock(&self.inner.modes, "gpu-display-modes")
            .get(scanout)
            .copied()
    }

    /// A copy of `scanout`'s current frame.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all, fields(scanout))]
    pub fn frame(&self, scanout: usize) -> Option<Frame> {
        self.with_frame(scanout, |frame| frame.clone())
    }

    /// `scanout`'s update counter, without copying pixels.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all, fields(scanout))]
    pub fn sequence(&self, scanout: usize) -> Option<u64> {
        self.with_frame(scanout, |frame| frame.sequence)
    }

    pub(crate) fn modes(&self) -> Vec<(u32, u32)> {
        ternvale_vmm::lockwatch::lock(&self.inner.modes, "gpu-display-modes").clone()
    }

    pub(crate) fn with_frame<R>(
        &self,
        scanout: usize,
        f: impl FnOnce(&mut Frame) -> R,
    ) -> Option<R> {
        let slot = self.inner.frames.get(scanout)?;
        let mut frame = ternvale_vmm::lockwatch::lock(slot, "gpu-scanout-frame");
        Some(f(&mut frame))
    }
}

/// Device-side state of one scanout.
#[derive(Debug, Clone, Default)]
pub struct ScanoutState {
    /// Resource shown, 0 when disabled.
    pub resource_id: u32,
    /// Part of the resource shown.
    pub rect: Rect,
    /// When the last flush reached this scanout.
    pub last_flush: Option<Instant>,
}

/// Point `frame` at `rect` of `resource` and copy it in.
pub(crate) fn show(frame: &mut Frame, resource: &Resource, rect: Rect) {
    frame.width = rect.width;
    frame.height = rect.height;
    frame.format = Some(resource.format);
    frame.pixels = vec![0; frame.stride() * rect.height as usize];
    copy_into(frame, resource, rect, rect);
    frame.sequence += 1;
    frame.updated = Some(Instant::now());
}

/// Blank `frame` (scanout disabled).
pub(crate) fn blank(frame: &mut Frame) {
    let sequence = frame.sequence + 1;
    *frame = Frame {
        sequence,
        updated: Some(Instant::now()),
        ..Frame::default()
    };
}

/// Copy `damage` (inside both `resource` and `view`) from `resource` into
/// `frame`, which shows `view` of the resource. Returns the bytes copied.
pub(crate) fn copy_into(frame: &mut Frame, resource: &Resource, view: Rect, damage: Rect) -> u64 {
    let src_stride = resource.stride();
    let dst_stride = frame.stride();
    let bpp = BYTES_PER_PIXEL as usize;
    let row = damage.width as usize * bpp;
    for h in 0..damage.height as usize {
        let sy = damage.y as usize + h;
        let src = sy * src_stride + damage.x as usize * bpp;
        let dst = (sy - view.y as usize) * dst_stride + (damage.x - view.x) as usize * bpp;
        frame.pixels[dst..dst + row].copy_from_slice(&resource.pixels[src..src + row]);
    }
    row as u64 * u64::from(damage.height)
}

#[cfg(test)]
#[path = "scanout_tests.rs"]
mod tests;
