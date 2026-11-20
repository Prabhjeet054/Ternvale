//! Per-window pixel capture for seamless mode.
//!
//! Preferred API is Windows Graphics Capture (occluded windows; Microsoft
//! Learn: `Windows.Graphics.Capture`, Windows 10 1803+). TODO(verify):
//! performance from a Rust Win32 agent, WinRT dispatcher requirements, and
//! whether a dedicated vsock / shared-memory ring is needed vs `window_frame`
//! JSON-hex chunks.
//!
//! This module uses `PrintWindow` + `GetDIBits` as a GDI stand-in so the
//! protocol can be exercised without WinRT. Occluded and many UWP windows
//! may capture as empty.

use ternvale_agent_proto::{GuestWindow, HexBytes, Message, MAX_BIN_CHUNK};

/// Capture RGB24 pixels for `window` when capture is enabled.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id = window.id, width = window.width, height = window.height))]
pub fn capture_rgb24(window: &GuestWindow) -> Option<Vec<u8>> {
    if !capture_enabled() {
        return None;
    }
    #[cfg(windows)]
    {
        return gdi_print_window(window);
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        None
    }
}

/// `TERNVALE_SEAMLESS_CAPTURE=1` turns on guest-side capture (default off:
/// host crops the shared framebuffer like Step 58).
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn capture_enabled() -> bool {
    std::env::var_os("TERNVALE_SEAMLESS_CAPTURE").is_some()
}

/// Split a buffer into `window_frame` messages (no pixel logging).
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id, seq, bytes = data.len(), width, height))]
pub fn frame_messages(id: u32, seq: u64, width: u32, height: u32, data: &[u8]) -> Vec<Message> {
    let mut out = Vec::new();
    let mut offset = 0u64;
    while offset < data.len() as u64 {
        let start = offset as usize;
        let end = (start + MAX_BIN_CHUNK).min(data.len());
        let last = end == data.len();
        out.push(Message::WindowFrame {
            id,
            seq,
            offset,
            last,
            width,
            height,
            format: "rgb24".into(),
            data: HexBytes(data[start..end].to_vec()),
        });
        offset = end as u64;
    }
    tracing::debug!(
        target: "ternvale::agent",
        id,
        seq,
        chunks = out.len(),
        bytes = data.len(),
        "window_frame chunks prepared (pixels not logged)"
    );
    out
}

#[cfg(windows)]
fn gdi_print_window(window: &GuestWindow) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
        ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::PrintWindow;

    let hwnd = window.id as HWND;
    let w = window.width.min(1920);
    let h = window.height.min(1080);
    if w == 0 || h == 0 {
        return None;
    }
    // SAFETY: GDI objects created here are selected/deselected and deleted
    // before return; `GetDC` is released; `PrintWindow` uses a valid HWND.
    unsafe {
        let hdc = GetDC(hwnd);
        if hdc == 0 {
            tracing::warn!(target: "ternvale::agent", id = window.id, "GetDC failed for window capture");
            return None;
        }
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, w as i32, h as i32);
        if mem == 0 || bmp == 0 {
            if bmp != 0 {
                DeleteObject(bmp);
            }
            if mem != 0 {
                DeleteDC(mem);
            }
            ReleaseDC(hwnd, hdc);
            return None;
        }
        let old = SelectObject(mem, bmp);
        let printed = PrintWindow(hwnd, mem, 0);
        let mut info = std::mem::zeroed::<BITMAPINFO>();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w as i32;
        info.bmiHeader.biHeight = -(h as i32);
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 24;
        info.bmiHeader.biCompression = BI_RGB;
        let stride = ((w * 3 + 3) / 4) * 4;
        let mut bgra = vec![0u8; (stride * h) as usize];
        let got = GetDIBits(
            mem,
            bmp,
            0,
            h,
            bgra.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        );
        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
        ReleaseDC(hwnd, hdc);
        if printed == 0 || got == 0 {
            tracing::warn!(
                target: "ternvale::agent",
                id = window.id,
                printed,
                got,
                "PrintWindow/GetDIBits failed (WGC not wired; occluded windows may be empty)"
            );
            return None;
        }
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            let row = (y * stride) as usize;
            for x in 0..w {
                let i = row + (x * 3) as usize;
                rgb.push(bgra[i + 2]);
                rgb.push(bgra[i + 1]);
                rgb.push(bgra[i]);
            }
        }
        tracing::debug!(
            target: "ternvale::agent",
            id = window.id,
            bytes = rgb.len(),
            "GDI window capture ready (pixels not logged)"
        );
        Some(rgb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternvale_agent_proto::GuestWindow;

    #[test]
    fn chunks_cover_payload_without_overlap() {
        let data = vec![1u8, 2, 3, 4];
        let msgs = frame_messages(7, 1, 2, 2, &data);
        assert_eq!(msgs.len(), 1);
        match &msgs[0] {
            Message::WindowFrame {
                id,
                last,
                data,
                format,
                ..
            } => {
                assert_eq!(*id, 7);
                assert!(*last);
                assert_eq!(data.0, vec![1, 2, 3, 4]);
                assert_eq!(format, "rgb24");
            }
            _ => panic!("expected window_frame"),
        }
    }

    #[test]
    fn capture_off_by_default() {
        let w = GuestWindow {
            id: 1,
            title: "x".into(),
            width: 8,
            height: 8,
            ..GuestWindow::default()
        };
        let prev = std::env::var_os("TERNVALE_SEAMLESS_CAPTURE");
        std::env::remove_var("TERNVALE_SEAMLESS_CAPTURE");
        assert!(!capture_enabled());
        assert!(capture_rgb24(&w).is_none());
        if let Some(v) = prev {
            std::env::set_var("TERNVALE_SEAMLESS_CAPTURE", v);
        }
    }
}
