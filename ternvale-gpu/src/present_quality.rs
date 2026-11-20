//! Per-VM Metal presentation quality (Step 109): scale, color space, layout.

/// Host `CAMetalLayer` color space. Guest pixels stay sRGB UNORM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PresentColor {
    /// `CGColorSpace.sRGB` (guest assumption).
    #[default]
    Srgb,
    /// Display P3 layer; sRGB samples are displayed in P3 (slightly wider).
    DisplayP3,
}

/// How many host windows to open for virtio-gpu scanouts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PresentLayout {
    /// One window (scanout 0, plus extra scanouts if the viewer requests them).
    #[default]
    One,
    /// One window per scanout.
    PerScanout,
}

/// Quality knobs the presenter copies onto each frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentQuality {
    /// `100` = 1×, `200` = 2× Retina. EDID DPI is `96 * scale_x100 / 100`.
    pub scale_x100: u32,
    /// Host layer color space.
    pub color: PresentColor,
    /// Multi-scanout windows.
    pub layout: PresentLayout,
}

impl Default for PresentQuality {
    fn default() -> Self {
        Self {
            scale_x100: 100,
            color: PresentColor::Srgb,
            layout: PresentLayout::One,
        }
    }
}

impl PresentQuality {
    /// `96` or `192` DPI for EDID physical size.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn dpi(&self) -> u32 {
        96 * self.scale_x100.max(1) / 100
    }

    /// True when the Metal layer should use Display P3.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn display_p3(&self) -> bool {
        self.color == PresentColor::DisplayP3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_follows_scale() {
        assert_eq!(PresentQuality::default().dpi(), 96);
        let hidpi = PresentQuality {
            scale_x100: 200,
            ..PresentQuality::default()
        };
        assert_eq!(hidpi.dpi(), 192);
    }
}
