//! Windows `CF_UNICODETEXT` clipboard with `AddClipboardFormatListener`.
//!
//! Own `SetClipboardData` writes are ignored (sequence number + last-set
//! text) so host→guest→host does not loop. Text larger than
//! [`crate::clip::MAX_CLIPBOARD_BYTES`] is dropped.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::clip;
use crate::AgentError;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
#[cfg(windows)]
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, SetClipboardData, CF_UNICODETEXT,
};
#[cfg(windows)]
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW, KillTimer,
    RegisterClassW, SetTimer, SetWindowLongPtrW, TranslateMessage, GWLP_USERDATA, HWND_MESSAGE,
    MSG, WM_CLIPBOARDUPDATE, WM_DESTROY, WM_TIMER, WNDCLASSW,
};

#[cfg_attr(not(windows), allow(dead_code))]
const POLL_MS: u32 = 250;

/// Shared guest clipboard state for the agent session and the listener thread.
#[derive(Debug)]
pub struct Hub {
    last_set: Mutex<Option<String>>,
    pending: Mutex<Option<String>>,
    #[cfg_attr(not(windows), allow(dead_code))]
    ignore_seq: AtomicU32,
    stop: AtomicBool,
}

impl Hub {
    /// Start the listener thread (best-effort; polling still works if it fails).
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
    pub fn start() -> Arc<Self> {
        let hub = Arc::new(Self {
            last_set: Mutex::new(None),
            pending: Mutex::new(None),
            ignore_seq: AtomicU32::new(0),
            stop: AtomicBool::new(false),
        });
        #[cfg(windows)]
        {
            let worker = Arc::clone(&hub);
            match std::thread::Builder::new()
                .name("ternvale-clip".into())
                .spawn(move || worker.listen())
            {
                Ok(_) => tracing::info!(target: "ternvale::agent", "clipboard listener started"),
                Err(error) => tracing::warn!(
                    target: "ternvale::agent",
                    error = %error,
                    "clipboard listener thread failed to start; polling only"
                ),
            }
        }
        hub
    }

    /// Host `ClipboardSet`: write `CF_UNICODETEXT` and remember it so the
    /// resulting `WM_CLIPBOARDUPDATE` is not pushed back.
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(len = text.len()))]
    pub fn set_text(&self, text: &str) -> Result<(), AgentError> {
        let Some(text) = clip::capped(text) else {
            return Ok(());
        };
        #[cfg(windows)]
        native_set(self, text)?;
        #[cfg(not(windows))]
        {
            if let Ok(mut last) = self.last_set.lock() {
                *last = Some(text.to_string());
            }
        }
        tracing::info!(target: "ternvale::agent", len = text.len(), "guest clipboard set");
        Ok(())
    }

    /// Current clipboard text (native, then last host-set value).
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn get_text(&self) -> Result<String, AgentError> {
        #[cfg(windows)]
        if let Some(text) = native_get() {
            return Ok(text);
        }
        Ok(self
            .last_set
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_default())
    }

    /// Guest→host text waiting since the last poll, if it was not our own set.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn take_pending(&self) -> Option<String> {
        #[cfg(windows)]
        self.poll_sequence();
        let text = self.pending.lock().ok().and_then(|mut g| g.take())?;
        let last = self.last_set.lock().ok().and_then(|g| g.clone());
        if clip::is_echo(last.as_deref(), &text) {
            tracing::trace!(target: "ternvale::agent", "clipboard push skipped (own set)");
            None
        } else {
            Some(text)
        }
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[cfg(windows)]
fn native_set(hub: &Hub, text: &str) -> Result<(), AgentError> {
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);
    let bytes = utf16.len() * 2;
    if bytes > crate::clip::MAX_CLIPBOARD_BYTES * 2 + 2 {
        return Ok(());
    }
    open_clipboard()?;
    // SAFETY: clipboard is open; `hmem` is a moveable HGLOBAL we own until
    // `SetClipboardData` takes it (or we unlock/close on failure).
    let result = unsafe {
        if EmptyClipboard() == 0 {
            CloseClipboard();
            return Err(last_action("EmptyClipboard"));
        }
        let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if hmem == 0 {
            CloseClipboard();
            return Err(last_action("GlobalAlloc"));
        }
        let ptr = GlobalLock(hmem);
        if ptr.is_null() {
            CloseClipboard();
            return Err(last_action("GlobalLock"));
        }
        std::ptr::copy_nonoverlapping(utf16.as_ptr().cast::<u8>(), ptr.cast::<u8>(), bytes);
        GlobalUnlock(hmem);
        if SetClipboardData(CF_UNICODETEXT, hmem) == 0 {
            CloseClipboard();
            return Err(last_action("SetClipboardData"));
        }
        CloseClipboard();
        Ok(())
    };
    result?;
    hub.ignore_seq
        .store(unsafe { GetClipboardSequenceNumber() }, Ordering::Release);
    if let Ok(mut last) = hub.last_set.lock() {
        *last = Some(text.to_string());
    }
    Ok(())
}

#[cfg(windows)]
fn native_get() -> Option<String> {
    if open_clipboard().is_err() {
        return None;
    }
    // SAFETY: clipboard is open; `GetClipboardData` returns a handle owned by
    // the clipboard — we lock it only while copying, then unlock and close.
    let text = unsafe {
        let handle = GetClipboardData(CF_UNICODETEXT);
        if handle == 0 {
            CloseClipboard();
            return None;
        }
        let ptr = GlobalLock(handle);
        if ptr.is_null() {
            CloseClipboard();
            return None;
        }
        let slice = wide_cstr(ptr.cast::<u16>());
        let text = String::from_utf16_lossy(slice);
        GlobalUnlock(handle);
        CloseClipboard();
        text
    };
    clip::capped(&text).map(str::to_string)
}

/// SAFETY: `ptr` is a NUL-terminated UTF-16 string from the clipboard.
#[cfg(windows)]
unsafe fn wide_cstr<'a>(ptr: *const u16) -> &'a [u16] {
    let mut len = 0usize;
    // Bound the scan so a hostile clipboard cannot hang the agent.
    while len < MAX_CLIPBOARD_BYTES + 1 && unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

#[cfg(windows)]
fn open_clipboard() -> Result<(), AgentError> {
    for _ in 0..8 {
        // SAFETY: `OpenClipboard(NULL)` claims the clipboard for this thread.
        if unsafe { OpenClipboard(0) } != 0 {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err(last_action("OpenClipboard"))
}

#[cfg(windows)]
fn last_action(action: &'static str) -> AgentError {
    AgentError::Action {
        action,
        source: std::io::Error::last_os_error(),
    }
}

#[cfg(windows)]
impl Hub {
    fn listen(&self) {
        let class: Vec<u16> = "TernvaleClipWnd\0".encode_utf16().collect();
        let wnd = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            lpszClassName: class.as_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        // SAFETY: `wnd` points at a valid class; `HWND_MESSAGE` is a message-only
        // parent; `GWLP_USERDATA` holds `self` for the life of this thread.
        let hwnd = unsafe {
            RegisterClassW(&wnd);
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                0,
                0,
                std::ptr::null(),
            )
        };
        if hwnd == 0 {
            tracing::warn!(
                target: "ternvale::agent",
                "AddClipboardFormatListener window failed; using sequence polling"
            );
            while !self.stop.load(Ordering::Acquire) {
                self.poll_sequence();
                std::thread::sleep(std::time::Duration::from_millis(u64::from(POLL_MS)));
            }
            return;
        }
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, self as *const Hub as isize);
            if AddClipboardFormatListener(hwnd) == 0 {
                tracing::warn!(
                    target: "ternvale::agent",
                    error = %std::io::Error::last_os_error(),
                    "AddClipboardFormatListener failed"
                );
            }
            SetTimer(hwnd, 1, POLL_MS, None);
        }
        tracing::info!(target: "ternvale::agent", "AddClipboardFormatListener registered");
        let mut msg = unsafe { std::mem::zeroed::<MSG>() };
        while !self.stop.load(Ordering::Acquire) {
            // SAFETY: `GetMessageW` writes `msg`; we dispatch until stop.
            let got = unsafe { GetMessageW(&mut msg, hwnd, 0, 0) };
            if got <= 0 {
                break;
            }
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        unsafe {
            KillTimer(hwnd, 1);
        }
    }

    fn poll_sequence(&self) {
        let seq = unsafe { GetClipboardSequenceNumber() };
        if seq == 0 || seq == self.ignore_seq.load(Ordering::Acquire) {
            return;
        }
        let Some(text) = native_get() else {
            return;
        };
        let last = self.last_set.lock().ok().and_then(|g| g.clone());
        if clip::is_echo(last.as_deref(), &text) {
            tracing::trace!(target: "ternvale::agent", "clipboard update ignored (own set)");
            self.ignore_seq.store(seq, Ordering::Release);
            return;
        }
        self.ignore_seq.store(seq, Ordering::Release);
        if let Ok(mut pending) = self.pending.lock() {
            *pending = Some(text);
        }
        tracing::debug!(target: "ternvale::agent", "guest clipboard changed");
    }
}

#[cfg(windows)]
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE || msg == WM_TIMER {
        let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
        if ptr != 0 {
            let hub = unsafe { &*(ptr as *const Hub) };
            hub.poll_sequence();
        }
        return 0;
    }
    if msg == WM_DESTROY {
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_stores_pending_without_echoing_last_set() {
        let hub = Hub {
            last_set: Mutex::new(Some("same".into())),
            pending: Mutex::new(None),
            ignore_seq: AtomicU32::new(0),
            stop: AtomicBool::new(false),
        };
        assert!(clip::is_echo(
            hub.last_set.lock().expect("lock").as_deref(),
            "same"
        ));
        *hub.pending.lock().expect("pending") = Some("from-guest".into());
        assert_eq!(hub.take_pending().as_deref(), Some("from-guest"));
        assert!(hub.take_pending().is_none());
        *hub.pending.lock().expect("pending") = Some("same".into());
        assert!(
            hub.take_pending().is_none(),
            "own set must not be pushed back to the host"
        );
    }
}
