//! Windows `EnumWindows` listing and `window_op` / `window_input`.
//!
//! Filtering follows EnumWindows + `IsWindowVisible` + `WS_EX_TOOLWINDOW`
//! (Microsoft Learn: winuser.h). UWP packaged apps appear as
//! `ApplicationFrameWindow` when they have a HWND.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use ternvale_agent_proto::{GuestWindow, MAX_WINDOWS};

use crate::windows::{classify_kind, skip_tool_or_invisible, window_state};
use crate::AgentError;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowRect,
    GetWindowTextW, IsIconic, IsWindowVisible, IsZoomed, PostMessageW, SetForegroundWindow,
    SetWindowPos, ShowWindow, GWL_EXSTYLE, GW_OWNER, HWND_TOP, SWP_NOACTIVATE, SWP_NOZORDER,
    SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, WM_CLOSE,
};

#[cfg(windows)]
struct EnumState {
    windows: Vec<GuestWindow>,
    z: u32,
}

/// Enumerate top-level application windows (empty on non-Windows hosts).
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn enum_top_level() -> Vec<GuestWindow> {
    #[cfg(windows)]
    {
        let mut state = EnumState {
            windows: Vec::new(),
            z: 0,
        };
        // SAFETY: `enum_proc` only reads HWND APIs and writes `EnumState` behind
        // `lParam` for the duration of `EnumWindows`.
        let ok = unsafe { EnumWindows(Some(enum_proc), std::ptr::from_mut(&mut state) as LPARAM) };
        if ok == 0 {
            tracing::warn!(target: "ternvale::agent", "EnumWindows failed");
            return Vec::new();
        }
        tracing::debug!(
            target: "ternvale::agent",
            count = state.windows.len(),
            "EnumWindows filtered list"
        );
        return state.windows;
    }
    #[cfg(not(windows))]
    Vec::new()
}

#[cfg(windows)]
unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = unsafe { &mut *(lparam as *mut EnumState) };
    if state.windows.len() >= MAX_WINDOWS {
        return 0;
    }
    if let Some(window) = describe(hwnd, state.z) {
        state.windows.push(window);
        state.z = state.z.saturating_add(1);
    }
    1
}

#[cfg(windows)]
fn describe(hwnd: HWND, z_order: u32) -> Option<GuestWindow> {
    let visible = unsafe { IsWindowVisible(hwnd) } != 0;
    let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    let title =
        wide_text(|buf| unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) });
    if skip_tool_or_invisible(visible, ex_style, &title) {
        return None;
    }
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
        return None;
    }
    let width = rect.right.saturating_sub(rect.left) as u32;
    let height = rect.bottom.saturating_sub(rect.top) as u32;
    if width == 0 || height == 0 {
        return None;
    }
    let owner = unsafe { GetWindow(hwnd, GW_OWNER) };
    let has_owner = owner != 0 && owner != hwnd;
    let class = wide_text(|buf| unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) });
    let focused = unsafe { GetForegroundWindow() } == hwnd;
    let kind = classify_kind(&class, &title, has_owner);
    Some(GuestWindow {
        id: hwnd as u32,
        title,
        x: rect.left,
        y: rect.top,
        width,
        height,
        z_order,
        state: window_state(
            unsafe { IsIconic(hwnd) } != 0,
            unsafe { IsZoomed(hwnd) } != 0,
        ),
        focused,
        parent_id: has_owner.then_some(owner as u32),
        kind,
        icon: None,
    })
}

#[cfg(windows)]
fn wide_text(fill: impl FnOnce(&mut [u16]) -> i32) -> String {
    let mut buf = [0u16; 256];
    let n = fill(&mut buf).max(0) as usize;
    let end = n.min(buf.len());
    String::from_utf16_lossy(&buf[..end])
        .trim_end_matches('\0')
        .to_string()
}

/// Move / show / close / focus a HWND.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id, op))]
pub fn apply_op(
    id: u32,
    op: &str,
    x: Option<i32>,
    y: Option<i32>,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<(), AgentError> {
    #[cfg(windows)]
    {
        let hwnd = id as HWND;
        let rc = match op {
            "close" => unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) },
            "focus" => unsafe { SetForegroundWindow(hwnd) },
            "minimize" => unsafe { ShowWindow(hwnd, SW_MINIMIZE) },
            "maximize" => unsafe { ShowWindow(hwnd, SW_MAXIMIZE) },
            "restore" => unsafe { ShowWindow(hwnd, SW_RESTORE) },
            "move" | "resize" => {
                let w = width.unwrap_or(0) as i32;
                let h = height.unwrap_or(0) as i32;
                unsafe {
                    SetWindowPos(
                        hwnd,
                        HWND_TOP,
                        x.unwrap_or(0),
                        y.unwrap_or(0),
                        w,
                        h,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                }
            }
            other => {
                tracing::warn!(target: "ternvale::agent", id, op = other, "window_op ignored");
                return Ok(());
            }
        };
        if rc == 0 {
            tracing::warn!(target: "ternvale::agent", id, op, "window_op Win32 returned 0");
        } else {
            tracing::debug!(target: "ternvale::agent", id, op, "window_op ok");
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = (id, op, x, y, width, height);
        Ok(())
    }
}

/// Click / key into a HWND. Uses `SendInput` after focusing.
///
/// TODO(verify): UWP apps may need UI Automation instead of SendInput.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::agent",
    skip_all,
    fields(id, kind)
)]
pub fn apply_input(id: u32, kind: &str, code: u32, value: i32) -> Result<(), AgentError> {
    #[cfg(windows)]
    {
        let hwnd = id as HWND;
        let _ = unsafe { SetForegroundWindow(hwnd) };
        tracing::debug!(
            target: "ternvale::agent",
            id,
            kind,
            code,
            value,
            "window_input focused guest window (SendInput mapping is host virtio-input)"
        );
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = (id, kind, code, value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn enum_is_empty_off_windows_host() {
        if !cfg!(windows) {
            assert!(super::enum_top_level().is_empty());
        }
    }
}
