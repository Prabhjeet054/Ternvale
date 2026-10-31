//! Guest window geometry for experimental seamless mode.

use serde::{Deserialize, Serialize};

/// Maximum windows in one `WindowListOk`.
pub const MAX_WINDOWS: usize = 64;
/// Longest window title on the wire.
pub const MAX_WINDOW_TITLE: usize = 256;
/// Largest reported width or height.
pub const MAX_WINDOW_DIMENSION: u32 = 16_384;

/// One top-level guest window in guest screen coordinates (origin top-left).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestWindow {
    /// Stable id from the guest (e.g. X11 window id).
    pub id: u32,
    /// UTF-8 title; may be empty.
    pub title: String,
    /// Left edge in guest pixels.
    pub x: i32,
    /// Top edge in guest pixels.
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl GuestWindow {
    /// Range checks for wire-supplied fields.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(id = self.id))]
    pub fn validate(&self) -> Result<(), String> {
        if self.title.len() > MAX_WINDOW_TITLE {
            return Err(format!("window title longer than {MAX_WINDOW_TITLE} bytes"));
        }
        if !(1..=MAX_WINDOW_DIMENSION).contains(&self.width)
            || !(1..=MAX_WINDOW_DIMENSION).contains(&self.height)
        {
            return Err(format!(
                "window {} size {}x{} outside 1..={MAX_WINDOW_DIMENSION}",
                self.id, self.width, self.height
            ));
        }
        Ok(())
    }
}

/// Validate a full list before encoding.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(count = windows.len()))]
pub fn validate_windows(windows: &[GuestWindow]) -> Result<(), String> {
    if windows.len() > MAX_WINDOWS {
        return Err(format!("window list longer than {MAX_WINDOWS}"));
    }
    for window in windows {
        window.validate()?;
    }
    Ok(())
}
