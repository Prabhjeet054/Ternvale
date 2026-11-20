//! `[gpu]`: virtio-gpu 3D opt-in and native display quality (Step 109).

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::vm::VmConfig;

/// Host Metal color space for the native presenter. The guest framebuffer is
/// always encoded as **sRGB** (virtio-gpu UNORM 8-bit). Display P3 only
/// changes the `CAMetalLayer` color space on the host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GpuColorSpace {
    /// `CGColorSpace.sRGB` / `bgra8Unorm_srgb` (default).
    #[default]
    Srgb,
    /// `CGColorSpace.displayP3`. Guest pixels are still sRGB samples.
    DisplayP3,
}

/// How the host opens windows for virtio-gpu scanouts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GpuWindows {
    /// One host window; extra scanouts share it (scanout 0 is shown unless
    /// the viewer opens more).
    #[default]
    One,
    /// One host window per enabled scanout.
    PerScanout,
}

/// The `[gpu]` table. Omitted: 2D scanout only (`gpu3d = false`), 1× scale, sRGB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct GpuSection {
    /// Host Venus renderer (virtio-gpu blobs/contexts/`SUBMIT_3D`). Default false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gpu3d: bool,
    /// Guest pixel scale vs host points (`1` or `2`). Default 1. EDID physical
    /// size uses `96 × scale` DPI so Windows/Linux pick ~96 or ~192 DPI.
    #[serde(default, skip_serializing_if = "scale_is_default")]
    pub scale: Option<u32>,
    /// Host presentation color space. Default sRGB.
    #[serde(default, skip_serializing_if = "is_srgb")]
    pub color_space: GpuColorSpace,
    /// Multi-scanout window layout. Default one window.
    #[serde(default, skip_serializing_if = "is_one_window")]
    pub windows: GpuWindows,
    /// Number of virtio-gpu scanouts (`1..=16`). Default 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub displays: Option<u32>,
}

fn scale_is_default(value: &Option<u32>) -> bool {
    matches!(value, None | Some(1))
}

fn is_srgb(value: &GpuColorSpace) -> bool {
    *value == GpuColorSpace::Srgb
}

fn is_one_window(value: &GpuWindows) -> bool {
    *value == GpuWindows::One
}

impl GpuSection {
    /// `1` or `2`.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn scale(&self) -> u32 {
        self.scale.unwrap_or(1)
    }

    /// Scanout count, default 1.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn display_count(&self) -> u32 {
        self.displays.unwrap_or(1)
    }

    /// Reject scale/display values the presenter cannot honour.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn validate(&self) -> Result<(), ConfigError> {
        let scale = self.scale();
        if scale != 1 && scale != 2 {
            return Err(ConfigError::InvalidGpu {
                field: "gpu.scale",
                reason: format!("must be 1 or 2, got {scale}"),
            });
        }
        let displays = self.display_count();
        if displays == 0 || displays > 16 {
            return Err(ConfigError::InvalidGpu {
                field: "gpu.displays",
                reason: format!("must be 1..=16, got {displays}"),
            });
        }
        Ok(())
    }
}

impl VmConfig {
    /// `[gpu]`, or defaults (3D off) when the table is absent.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name))]
    pub fn gpu_section(&self) -> GpuSection {
        self.gpu.clone().unwrap_or_default()
    }

    /// Per-VM 3D flag. Default **false** even when the VMM is built with `gpu-3d`.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name))]
    pub fn gpu3d(&self) -> bool {
        self.gpu.as_ref().is_some_and(|gpu| gpu.gpu3d)
    }
}

#[cfg(test)]
mod tests {
    use super::{GpuColorSpace, GpuSection, GpuWindows};

    #[test]
    fn gpu3d_defaults_false() {
        let gpu = GpuSection::default();
        assert!(!gpu.gpu3d);
        assert_eq!(gpu.scale(), 1);
        assert_eq!(gpu.color_space, GpuColorSpace::Srgb);
        assert_eq!(gpu.windows, GpuWindows::One);
        assert_eq!(gpu.display_count(), 1);
    }

    #[test]
    fn scale_two_and_p3_validate() {
        let gpu = GpuSection {
            scale: Some(2),
            color_space: GpuColorSpace::DisplayP3,
            windows: GpuWindows::PerScanout,
            displays: Some(2),
            ..GpuSection::default()
        };
        gpu.validate().expect("ok");
        assert_eq!(gpu.scale(), 2);
        assert!(GpuSection {
            scale: Some(3),
            ..GpuSection::default()
        }
        .validate()
        .is_err());
    }
}
