//! Guest window geometry for experimental seamless mode.

use serde::{Deserialize, Serialize};

use crate::HexBytes;

/// Maximum windows in one `WindowListOk`.
pub const MAX_WINDOWS: usize = 64;
/// Longest window title on the wire.
pub const MAX_WINDOW_TITLE: usize = 256;
/// Largest reported width or height.
pub const MAX_WINDOW_DIMENSION: u32 = 16_384;
/// Largest optional window icon (PNG/BMP bytes).
pub const MAX_WINDOW_ICON: usize = 8_192;
/// Largest assembled per-window RGB24 frame (guest capture).
pub const MAX_WINDOW_FRAME_BYTES: usize = 2 * 1024 * 1024;

/// One top-level guest window in guest screen coordinates (origin top-left).
///
/// Additive optional fields (`z_order`, `state`, `focused`, `parent_id`,
/// `kind`, `icon`) are omitted when default so existing `WindowListOk`
/// goldens stay byte-identical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GuestWindow {
    /// Stable id from the guest (X11 id or HWND).
    pub id: u32,
    /// UTF-8 title; may be empty.
    pub title: String,
    /// Left edge in guest pixels.
    pub x: i32,
    /// Top edge in guest pixels.
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Front-to-back index from `EnumWindows` / stacking order. Omitted if 0.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub z_order: u32,
    /// Minimized / maximized / normal.
    #[serde(default, skip_serializing_if = "WindowState::is_normal")]
    pub state: WindowState,
    /// True when this is the foreground window.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub focused: bool,
    /// Owner / parent HWND for dialogs and popups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<u32>,
    /// How the host should present this window.
    #[serde(default, skip_serializing_if = "WindowKind::is_app")]
    pub kind: WindowKind,
    /// Small icon bytes (never logged as pixels; `HexBytes` Debug is length).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<HexBytes>,
}

/// Shown state of a guest window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WindowState {
    /// Neither minimized nor maximized.
    #[default]
    Normal,
    /// Iconic / minimized.
    Minimized,
    /// Zoomed / maximized.
    Maximized,
}

impl WindowState {
    /// Wire / log name.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Minimized => "minimized",
            Self::Maximized => "maximized",
        }
    }

    fn is_normal(&self) -> bool {
        matches!(self, Self::Normal)
    }
}

/// Role of a guest window for host mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    /// Ordinary application window (default).
    #[default]
    App,
    /// Modal or owned dialog.
    Dialog,
    /// Menu / combo / tooltip popup.
    Popup,
    /// Shell taskbar (`Shell_TrayWnd`). Map to host UI; do not open a native window.
    Taskbar,
    /// Start menu / launcher. Map to host UI.
    Start,
}

impl WindowKind {
    /// Wire / log name.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Dialog => "dialog",
            Self::Popup => "popup",
            Self::Taskbar => "taskbar",
            Self::Start => "start",
        }
    }

    fn is_app(&self) -> bool {
        matches!(self, Self::App)
    }
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
        if let Some(icon) = &self.icon {
            if icon.0.len() > MAX_WINDOW_ICON {
                return Err(format!("window icon longer than {MAX_WINDOW_ICON} bytes"));
            }
        }
        if let Some(parent) = self.parent_id {
            if parent == self.id {
                return Err("window parent_id must not equal id".into());
            }
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

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// Host → guest geometry / chrome operation (`window_op`).
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(id, op))]
pub fn validate_window_op(
    id: u32,
    op: &str,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<(), String> {
    if id == 0 {
        return Err("window_op id must be non-zero".into());
    }
    match op {
        "move" | "resize" | "minimize" | "maximize" | "restore" | "close" | "focus" => {}
        _ => return Err(format!("window_op unknown op {op}")),
    }
    if matches!(op, "move" | "resize") {
        if let Some(w) = width {
            if !(1..=MAX_WINDOW_DIMENSION).contains(&w) {
                return Err("window_op width out of range".into());
            }
        }
        if let Some(h) = height {
            if !(1..=MAX_WINDOW_DIMENSION).contains(&h) {
                return Err("window_op height out of range".into());
            }
        }
    }
    Ok(())
}

/// Host → guest pointer/key routed to a window.
#[tracing::instrument(
    level = "trace",
    target = "ternvale::agent",
    skip_all,
    fields(id, kind)
)]
pub fn validate_window_input(id: u32, kind: &str) -> Result<(), String> {
    if id == 0 {
        return Err("window_input id must be non-zero".into());
    }
    match kind {
        "key" | "button" | "move" | "abs" => Ok(()),
        _ => Err(format!("window_input unknown kind {kind}")),
    }
}

/// Agent → host per-window capture chunk (never log `data`).
#[tracing::instrument(
    level = "trace",
    target = "ternvale::agent",
    skip_all,
    fields(id, seq, offset, last, width, height, bytes)
)]
pub fn validate_window_frame(
    id: u32,
    offset: u64,
    width: u32,
    height: u32,
    format: &str,
    data_len: usize,
) -> Result<(), String> {
    if id == 0 {
        return Err("window_frame id must be non-zero".into());
    }
    if width == 0 || height == 0 || width > MAX_WINDOW_DIMENSION || height > MAX_WINDOW_DIMENSION {
        return Err("window_frame geometry out of range".into());
    }
    if format != "rgb24" {
        return Err("window_frame format must be rgb24".into());
    }
    if data_len > crate::MAX_BIN_CHUNK {
        return Err(format!(
            "window_frame longer than {} bytes",
            crate::MAX_BIN_CHUNK
        ));
    }
    let end = offset
        .checked_add(data_len as u64)
        .ok_or_else(|| "window_frame offset overflow".to_string())?;
    if end > MAX_WINDOW_FRAME_BYTES as u64 {
        return Err(format!(
            "window_frame stream longer than {MAX_WINDOW_FRAME_BYTES} bytes"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_fields_are_omitted_from_json() {
        let window = GuestWindow {
            id: 1,
            title: "A".into(),
            x: 0,
            y: 0,
            width: 10,
            height: 10,
            ..GuestWindow::default()
        };
        let json = serde_json::to_string(&window).expect("json");
        assert_eq!(
            json,
            r#"{"id":1,"title":"A","x":0,"y":0,"width":10,"height":10}"#
        );
    }

    #[test]
    fn additive_fields_round_trip() {
        let window = GuestWindow {
            id: 2,
            title: "dlg".into(),
            x: 1,
            y: 2,
            width: 100,
            height: 80,
            z_order: 3,
            state: WindowState::Maximized,
            focused: true,
            parent_id: Some(1),
            kind: WindowKind::Dialog,
            icon: Some(HexBytes(vec![0xaa, 0xbb])),
        };
        window.validate().expect("ok");
        let json = serde_json::to_string(&window).expect("json");
        let back: GuestWindow = serde_json::from_str(&json).expect("de");
        assert_eq!(back, window);
        assert!(json.contains("\"kind\":\"dialog\""));
    }

    #[test]
    fn rejects_too_many_windows_and_bad_icon() {
        let one = GuestWindow {
            id: 1,
            title: "x".into(),
            width: 1,
            height: 1,
            ..GuestWindow::default()
        };
        let many = vec![one.clone(); MAX_WINDOWS + 1];
        assert!(validate_windows(&many).is_err());
        let mut bad = one;
        bad.icon = Some(HexBytes(vec![0; MAX_WINDOW_ICON + 1]));
        assert!(bad.validate().is_err());
    }

    #[test]
    fn window_op_and_frame_bounds() {
        assert!(validate_window_op(1, "focus", None, None).is_ok());
        assert!(validate_window_op(0, "close", None, None).is_err());
        assert!(validate_window_op(1, "explode", None, None).is_err());
        assert!(validate_window_input(1, "abs").is_ok());
        assert!(validate_window_input(1, "warp").is_err());
        assert!(validate_window_frame(1, 0, 8, 8, "rgb24", 16).is_ok());
        assert!(validate_window_frame(1, 0, 8, 8, "nv12", 16).is_err());
        assert!(validate_window_frame(1, MAX_WINDOW_FRAME_BYTES as u64, 8, 8, "rgb24", 1).is_err());
    }
}
