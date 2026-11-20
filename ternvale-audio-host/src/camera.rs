//! Host webcam frames for the agent fallback (not UVC/xHCI).
//!
//! Frame payloads are never logged. A 16×16 RGB test pattern is used when
//! `TERNVALE_CAMERA_TEST` is set so unit tests do not open AVFoundation.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::permission::{probe_camera, PermissionDenied, CAMERA_TEST_ENV};

/// Packed RGB24 frame the agent can forward. `Debug` is size only.
pub struct CameraFrame {
    /// Frame counter.
    pub seq: u64,
    /// Pixels.
    pub width: u32,
    /// Pixels.
    pub height: u32,
    /// `rgb24`.
    pub format: &'static str,
    /// `width * height * 3` bytes. Never log this field.
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for CameraFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CameraFrame")
            .field("seq", &self.seq)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .field("pixels", &format_args!("[{} bytes]", self.pixels.len()))
            .finish()
    }
}

/// Pull frames for the guest agent.
pub trait CameraSource: Send {
    /// Next frame, or `None` if capture is idle.
    fn next_frame(&mut self) -> Option<CameraFrame>;
    /// Stop the sensor.
    fn close(&mut self);
    /// True while frames are being produced.
    fn streaming(&self) -> bool;
}

/// 16×16 RGB bars (not a live camera).
pub struct TestPatternCamera {
    seq: AtomicU64,
    live: AtomicBool,
    width: u32,
    height: u32,
}

impl TestPatternCamera {
    /// Small pattern for agent tests.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn new() -> Self {
        Self::sized(16, 16)
    }

    /// 1280×720 RGB24 (guest camera-app preview analog).
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn hd720() -> Self {
        Self::sized(1280, 720)
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu",
        skip_all,
        fields(width, height)
    )]
    fn sized(width: u32, height: u32) -> Self {
        tracing::debug!(
            target: "ternvale::gpu",
            width,
            height,
            "synthetic camera geometry (pixels not logged)"
        );
        Self {
            seq: AtomicU64::new(0),
            live: AtomicBool::new(true),
            width,
            height,
        }
    }
}

impl Default for TestPatternCamera {
    fn default() -> Self {
        Self::new()
    }
}

impl CameraSource for TestPatternCamera {
    fn next_frame(&mut self) -> Option<CameraFrame> {
        if !self.live.load(Ordering::Relaxed) {
            return None;
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let mut pixels = vec![0u8; (self.width * self.height * 3) as usize];
        for y in 0..self.height {
            for x in 0..self.width {
                let i = ((y * self.width + x) * 3) as usize;
                pixels[i] = (x * 16) as u8;
                pixels[i + 1] = (y * 16) as u8;
                pixels[i + 2] = 0x80;
            }
        }
        Some(CameraFrame {
            seq,
            width: self.width,
            height: self.height,
            format: "rgb24",
            pixels,
        })
    }

    fn close(&mut self) {
        self.live.store(false, Ordering::Relaxed);
    }

    fn streaming(&self) -> bool {
        self.live.load(Ordering::Relaxed)
    }
}

/// Open a camera after the permission probe. Live AVFoundation belongs to the
/// Mac app (TCC); this process uses a test pattern or fails closed.
#[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
pub fn open_camera() -> Result<Box<dyn CameraSource>, PermissionDenied> {
    probe_camera()?;
    if std::env::var_os(CAMERA_TEST_ENV).is_some() {
        tracing::info!(
            target: "ternvale::gpu",
            "camera source is a synthetic test pattern (not sensor pixels)"
        );
        return Ok(Box::new(TestPatternCamera::new()));
    }
    tracing::info!(
        target: "ternvale::gpu",
        "camera source is a synthetic pattern; live AVFoundation is the app TCC path"
    );
    Ok(Box::new(TestPatternCamera::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pattern_frame_is_sized_and_debug_hides_pixels() {
        let mut cam = TestPatternCamera::new();
        let frame = cam.next_frame().expect("frame");
        assert_eq!(frame.pixels.len(), 16 * 16 * 3);
        let text = format!("{frame:?}");
        assert!(text.contains("768 bytes"), "{text}");
        assert!(!text.contains("0x80"), "{text}");
        assert!(!text.contains("[0,"), "{text}");
    }

    #[test]
    fn hd720_preview_is_1280x720_and_debug_hides_pixels() {
        let mut cam = TestPatternCamera::hd720();
        let frame = cam.next_frame().expect("frame");
        assert_eq!(frame.width, 1280);
        assert_eq!(frame.height, 720);
        assert_eq!(frame.pixels.len(), 1280 * 720 * 3);
        let text = format!("{frame:?}");
        assert!(text.contains("2764800 bytes"), "{text}");
        assert!(!text.contains("0x80"), "{text}");
        cam.close();
        assert!(!cam.streaming());
    }
}
