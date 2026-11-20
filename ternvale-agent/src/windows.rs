//! Guest window enumeration (Linux `wmctrl`, Windows `EnumWindows`).

use ternvale_agent_proto::{GuestWindow, WindowKind, WindowState};

use crate::AgentError;

/// `WS_EX_TOOLWINDOW` (winuser.h) — tool windows are filtered out.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;

/// List mapped top-level windows for seamless mode.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn list_guest_windows() -> Result<Vec<GuestWindow>, AgentError> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(windows) = list_via_wmctrl() {
            tracing::info!(target: "ternvale::agent", count = windows.len(), "window list from wmctrl");
            return Ok(windows);
        }
        tracing::debug!(target: "ternvale::agent", "wmctrl unavailable; returning empty window list");
        return Ok(Vec::new());
    }
    #[cfg(windows)]
    {
        let windows = crate::win_windows::enum_top_level();
        tracing::info!(target: "ternvale::agent", count = windows.len(), "window list from EnumWindows");
        return Ok(windows);
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        tracing::warn!(target: "ternvale::agent", "window_list ignored on this guest OS");
        Ok(Vec::new())
    }
}

/// Apply a host `window_op` (move / resize / chrome / focus / close).
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id, op))]
pub fn apply_window_op(
    id: u32,
    op: &str,
    x: Option<i32>,
    y: Option<i32>,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<(), AgentError> {
    #[cfg(windows)]
    {
        return crate::win_windows::apply_op(id, op, x, y, width, height);
    }
    #[cfg(target_os = "linux")]
    {
        return apply_wmctrl_op(id, op, x, y, width, height);
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (x, y, width, height);
        tracing::warn!(target: "ternvale::agent", id, op, "window_op ignored on this OS");
        Ok(())
    }
}

/// Inject pointer/key into the named guest window.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::agent",
    skip_all,
    fields(id, kind, code, value)
)]
pub fn apply_window_input(id: u32, kind: &str, code: u32, value: i32) -> Result<(), AgentError> {
    #[cfg(windows)]
    {
        return crate::win_windows::apply_input(id, kind, code, value);
    }
    #[cfg(not(windows))]
    {
        tracing::debug!(
            target: "ternvale::agent",
            id,
            kind,
            code,
            value,
            "window_input not implemented on this OS (Linux uses host virtio-input)"
        );
        Ok(())
    }
}

/// Whether a top-level HWND should be omitted from the seamless list.
///
/// Filters invisible windows and `WS_EX_TOOLWINDOW` tool windows
/// (Microsoft Learn: EnumWindows / GetWindowLongW GWL_EXSTYLE).
#[cfg_attr(not(windows), allow(dead_code))]
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn skip_tool_or_invisible(visible: bool, ex_style: u32, title: &str) -> bool {
    if !visible {
        return true;
    }
    if ex_style & WS_EX_TOOLWINDOW != 0 {
        return true;
    }
    let _ = title;
    false
}

/// Classify a Win32 class / title for host mapping.
///
/// UWP/packaged apps often use `ApplicationFrameWindow` (Microsoft Learn:
/// ApplicationFrame). TODO(verify): CoreWindow HWNDs that EnumWindows misses.
#[cfg_attr(not(windows), allow(dead_code))]
#[tracing::instrument(
    level = "trace",
    target = "ternvale::agent",
    skip_all,
    fields(class, has_owner)
)]
pub fn classify_kind(class: &str, title: &str, has_owner: bool) -> WindowKind {
    let class_l = class.to_ascii_lowercase();
    let title_l = title.to_ascii_lowercase();
    if class_l == "shell_traywnd" || class_l == "shell_secondarytraywnd" {
        return WindowKind::Taskbar;
    }
    if class_l.contains("start") && title_l.contains("start") {
        return WindowKind::Start;
    }
    if has_owner {
        if class_l.contains("#32768") || class_l.contains("popup") {
            return WindowKind::Popup;
        }
        return WindowKind::Dialog;
    }
    if class_l == "applicationframewindow" {
        // Packaged / UWP top-level frame; still an app window.
        return WindowKind::App;
    }
    WindowKind::App
}

/// Shown state from iconic / zoomed flags.
#[cfg_attr(not(windows), allow(dead_code))]
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn window_state(iconic: bool, zoomed: bool) -> WindowState {
    if iconic {
        WindowState::Minimized
    } else if zoomed {
        WindowState::Maximized
    } else {
        WindowState::Normal
    }
}

#[cfg(target_os = "linux")]
fn list_via_wmctrl() -> Result<Vec<GuestWindow>, AgentError> {
    let out = std::process::Command::new("wmctrl")
        .args(["-lG"])
        .output()
        .map_err(|source| AgentError::Action {
            action: "wmctrl -lG",
            source,
        })?;
    if !out.status.success() {
        tracing::warn!(
            target: "ternvale::agent",
            code = ?out.status.code(),
            "wmctrl -lG failed"
        );
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(parse_wmctrl_lg(&text))
}

#[cfg(target_os = "linux")]
fn apply_wmctrl_op(
    id: u32,
    op: &str,
    x: Option<i32>,
    y: Option<i32>,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<(), AgentError> {
    let hex = format!("0x{id:x}");
    let mut cmd = std::process::Command::new("wmctrl");
    match op {
        "close" => {
            cmd.args(["-i", "-c", &hex]);
        }
        "focus" => {
            cmd.args(["-i", "-a", &hex]);
        }
        "minimize" => {
            cmd.args(["-i", "-r", &hex, "-b", "add,hidden"]);
        }
        "maximize" => {
            cmd.args(["-i", "-r", &hex, "-b", "add,maximized_vert,maximized_horz"]);
        }
        "restore" => {
            cmd.args([
                "-i",
                "-r",
                &hex,
                "-b",
                "remove,hidden,maximized_vert,maximized_horz",
            ]);
        }
        "move" | "resize" => {
            let geom = format!(
                "0,{},{},{},{}",
                x.unwrap_or(0),
                y.unwrap_or(0),
                width.unwrap_or(0),
                height.unwrap_or(0)
            );
            cmd.args(["-i", "-r", &hex, "-e", &geom]);
        }
        _ => {
            tracing::warn!(target: "ternvale::agent", id, op, "wmctrl window_op ignored");
            return Ok(());
        }
    }
    match cmd.status() {
        Ok(status) if status.success() => {
            tracing::debug!(target: "ternvale::agent", id, op, "wmctrl window_op ok");
            Ok(())
        }
        Ok(status) => {
            tracing::warn!(target: "ternvale::agent", id, op, code = ?status.code(), "wmctrl window_op failed");
            Ok(())
        }
        Err(source) => Err(AgentError::Action {
            action: "wmctrl window_op",
            source,
        }),
    }
}

/// Parse `wmctrl -lG` lines: id desktop x y width height host title...
#[cfg(any(target_os = "linux", test))]
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(bytes = text.len()))]
pub fn parse_wmctrl_lg(text: &str) -> Vec<GuestWindow> {
    let mut windows = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let Some(window) = parse_wmctrl_line(line) else {
            tracing::trace!(target: "ternvale::agent", line, "skipped wmctrl line");
            continue;
        };
        windows.push(window);
    }
    windows
}

#[cfg(any(target_os = "linux", test))]
fn parse_wmctrl_line(line: &str) -> Option<GuestWindow> {
    let mut parts = line.split_whitespace();
    let id_hex = parts.next()?;
    let id = u32::from_str_radix(id_hex.trim_start_matches("0x"), 16).ok()?;
    let _desktop = parts.next()?;
    let x: i32 = parts.next()?.parse().ok()?;
    let y: i32 = parts.next()?.parse().ok()?;
    let width: u32 = parts.next()?.parse().ok()?;
    let height: u32 = parts.next()?.parse().ok()?;
    let _host = parts.next()?;
    let title = parts.collect::<Vec<_>>().join(" ");
    if width == 0 || height == 0 {
        return None;
    }
    Some(GuestWindow {
        id,
        title,
        x,
        y,
        width,
        height,
        ..GuestWindow::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wmctrl_sample() {
        let sample = "0x01200003  0 10 20 640 480 host xterm\n";
        let list = parse_wmctrl_lg(sample);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, 0x0120_0003);
        assert_eq!(list[0].title, "xterm");
        assert_eq!(
            (list[0].x, list[0].y, list[0].width, list[0].height),
            (10, 20, 640, 480)
        );
    }

    #[test]
    fn parses_two_wmctrl_lines_as_separate_windows() {
        let sample = "\
0x00000001  0 0 0 640 480 host xterm\n\
0x00000002  0 700 0 800 600 host gedit\n";
        let list = parse_wmctrl_lg(sample);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].title, "xterm");
        assert_eq!(list[1].title, "gedit");
        assert_ne!(list[0].id, list[1].id);
    }

    #[test]
    fn filters_tool_and_invisible() {
        assert!(skip_tool_or_invisible(false, 0, "x"));
        assert!(skip_tool_or_invisible(true, WS_EX_TOOLWINDOW, "tip"));
        assert!(!skip_tool_or_invisible(true, 0, "Notepad"));
    }

    #[test]
    fn classifies_taskbar_uwp_and_dialogs() {
        assert_eq!(
            classify_kind("Shell_TrayWnd", "", false),
            WindowKind::Taskbar
        );
        assert_eq!(
            classify_kind("ApplicationFrameWindow", "Photos", false),
            WindowKind::App
        );
        assert_eq!(classify_kind("Notepad", "Open", true), WindowKind::Dialog);
        assert_eq!(window_state(true, false), WindowState::Minimized);
        assert_eq!(window_state(false, true), WindowState::Maximized);
    }
}
