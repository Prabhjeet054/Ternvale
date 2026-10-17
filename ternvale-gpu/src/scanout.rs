//! Host-side pixel buffers, one per scanout, and the handle that reads them.
//!
//! The control-queue worker writes a scanout's [`Frame`] on `SET_SCANOUT` and
//! `RESOURCE_FLUSH`; anything holding a [`GpuDisplay`] (the Step 44 presenter,
//! tests) reads a copy. `sequence` increases on every update, so a reader can
//! tell whether a frame is new without comparing pixels.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::protocol::Rect;
use crate::resource::{PixelFormat, Resource, BYTES_PER_PIXEL};

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
    modes: Arc<Vec<(u32, u32)>>,
    frames: Arc<Vec<Mutex<Frame>>>,
}

impl GpuDisplay {
    /// One disabled frame per mode (width, height) the device advertises.
    pub(crate) fn new(modes: &[(u32, u32)]) -> Self {
        Self {
            modes: Arc::new(modes.to_vec()),
            frames: Arc::new(modes.iter().map(|_| Mutex::new(Frame::default())).collect()),
        }
    }

    /// Number of scanouts.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn scanouts(&self) -> usize {
        self.modes.len()
    }

    /// The mode `GET_DISPLAY_INFO` reports for `scanout`.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all, fields(scanout))]
    pub fn mode(&self, scanout: usize) -> Option<(u32, u32)> {
        self.modes.get(scanout).copied()
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

    pub(crate) fn modes(&self) -> &[(u32, u32)] {
        &self.modes
    }

    pub(crate) fn with_frame<R>(
        &self,
        scanout: usize,
        f: impl FnOnce(&mut Frame) -> R,
    ) -> Option<R> {
        let slot = self.frames.get(scanout)?;
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
}

/// Blank `frame` (scanout disabled).
pub(crate) fn blank(frame: &mut Frame) {
    let sequence = frame.sequence + 1;
    *frame = Frame {
        sequence,
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
