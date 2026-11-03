//! macOS memory-pressure notifications (`dispatch_source` MEMORYPRESSURE).
//!
//! Raw `libSystem` dispatch FFI stays in this crate.

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, AtomicU8, Ordering};
use std::sync::Arc;

/// Host memory pressure as reported by `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MemoryPressure {
    /// `DISPATCH_MEMORYPRESSURE_NORMAL`.
    Normal = 1,
    /// `DISPATCH_MEMORYPRESSURE_WARN`.
    Warn = 2,
    /// `DISPATCH_MEMORYPRESSURE_CRITICAL`.
    Critical = 4,
}

impl MemoryPressure {
    fn from_bits(bits: usize) -> Self {
        if bits & DISPATCH_MEMORYPRESSURE_CRITICAL != 0 {
            Self::Critical
        } else if bits & DISPATCH_MEMORYPRESSURE_WARN != 0 {
            Self::Warn
        } else {
            Self::Normal
        }
    }

    /// Short name for logs and `query-stats`.
    #[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }
}

/// Live subscription. Dropping it cancels the dispatch source.
pub struct PressureWatch {
    source: *mut c_void,
    ctx: *mut WatchState,
}

// SAFETY: dispatch objects are process-wide; `WatchState` is `Send`.
unsafe impl Send for PressureWatch {}
unsafe impl Sync for PressureWatch {}

struct WatchState {
    source: AtomicPtr<c_void>,
    callback: Arc<dyn Fn(MemoryPressure) + Send + Sync>,
    last: AtomicU8,
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    static _dispatch_source_type_memorypressure: u8;
    fn dispatch_source_create(
        kind: *const c_void,
        handle: usize,
        mask: usize,
        queue: *mut c_void,
    ) -> *mut c_void;
    fn dispatch_get_global_queue(priority: isize, flags: usize) -> *mut c_void;
    fn dispatch_source_set_event_handler_f(
        source: *mut c_void,
        handler: Option<unsafe extern "C" fn(*mut c_void)>,
    );
    fn dispatch_set_context(object: *mut c_void, context: *mut c_void);
    fn dispatch_resume(object: *mut c_void);
    fn dispatch_release(object: *mut c_void);
    fn dispatch_source_cancel(source: *mut c_void);
    fn dispatch_source_get_data(source: *mut c_void) -> usize;
}

const DISPATCH_MEMORYPRESSURE_NORMAL: usize = 0x01;
const DISPATCH_MEMORYPRESSURE_WARN: usize = 0x02;
const DISPATCH_MEMORYPRESSURE_CRITICAL: usize = 0x04;

/// Subscribe to host memory-pressure events.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all)]
pub fn watch_memory_pressure(
    callback: Arc<dyn Fn(MemoryPressure) + Send + Sync>,
) -> Option<PressureWatch> {
    let mask = DISPATCH_MEMORYPRESSURE_NORMAL
        | DISPATCH_MEMORYPRESSURE_WARN
        | DISPATCH_MEMORYPRESSURE_CRITICAL;
    let state = Box::new(WatchState {
        source: AtomicPtr::new(std::ptr::null_mut()),
        callback,
        last: AtomicU8::new(MemoryPressure::Normal as u8),
    });
    let ctx = Box::into_raw(state);
    // SAFETY: `_dispatch_source_type_memorypressure` is the libSystem type
    // object for MEMORYPRESSURE; the global queue lives for the process.
    let source = unsafe {
        let queue = dispatch_get_global_queue(0, 0);
        let kind = std::ptr::addr_of!(_dispatch_source_type_memorypressure).cast::<c_void>();
        dispatch_source_create(kind, 0, mask, queue)
    };
    if source.is_null() {
        tracing::error!(
            target: "ternvale::hv",
            "dispatch_source_create(MEMORYPRESSURE) returned null"
        );
        // SAFETY: we own `ctx` and dispatch never took it.
        unsafe {
            drop(Box::from_raw(ctx));
        }
        return None;
    }
    // SAFETY: `ctx` is the WatchState we just boxed; `source` is live.
    unsafe {
        (*ctx).source.store(source, Ordering::Release);
        dispatch_set_context(source, ctx.cast());
        dispatch_source_set_event_handler_f(source, Some(pressure_event));
        dispatch_resume(source);
    }
    tracing::info!(target: "ternvale::hv", "watching macOS memory pressure");
    Some(PressureWatch { source, ctx })
}

unsafe extern "C" fn pressure_event(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    let state = ctx.cast::<WatchState>();
    // SAFETY: dispatch holds `ctx` until cancel; `source` was stored before resume.
    let source = unsafe { (*state).source.load(Ordering::Acquire) };
    if source.is_null() {
        return;
    }
    let bits = unsafe { dispatch_source_get_data(source) };
    let level = MemoryPressure::from_bits(bits);
    let prev = unsafe { (*state).last.swap(level as u8, Ordering::AcqRel) };
    if prev == level as u8 {
        return;
    }
    tracing::info!(
        target: "ternvale::hv",
        pressure = level.as_str(),
        bits,
        "host memory pressure changed"
    );
    (unsafe { &(*state).callback })(level);
}

impl Drop for PressureWatch {
    fn drop(&mut self) {
        if !self.source.is_null() {
            // SAFETY: `source` came from `dispatch_source_create`.
            unsafe {
                dispatch_source_cancel(self.source);
                dispatch_release(self.source);
            }
            self.source = std::ptr::null_mut();
        }
        if !self.ctx.is_null() {
            // SAFETY: no more dispatch callbacks after cancel+release.
            unsafe {
                drop(Box::from_raw(self.ctx));
            }
            self.ctx = std::ptr::null_mut();
        }
        tracing::debug!(target: "ternvale::hv", "stopped watching memory pressure");
    }
}

#[cfg(test)]
mod tests {
    use super::MemoryPressure;

    #[test]
    fn bits_prefer_critical_then_warn() {
        assert_eq!(MemoryPressure::from_bits(1), MemoryPressure::Normal);
        assert_eq!(MemoryPressure::from_bits(2), MemoryPressure::Warn);
        assert_eq!(MemoryPressure::from_bits(4 | 2), MemoryPressure::Critical);
        assert_eq!(MemoryPressure::Critical.as_str(), "critical");
    }
}
