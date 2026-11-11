//! Live VZ session (C ABI) used by [`crate`] callers.

use std::os::raw::{c_char, c_void};
use std::sync::{Arc, Mutex};

use crate::error::VzError;
use crate::ffi::{self, Handle};
use crate::plan::VzPlan;

/// Callback from Swift (VM queue). Must not re-enter VZ via `queue.sync`.
pub type StateHook = Arc<dyn Fn(i32, String) + Send + Sync>;

struct LiveInner {
    handle: Handle,
    ctx: *mut c_void,
}

// SAFETY: `ctx` is only deref'd from the Swift VM queue or Drop after
// destroy; the boxed hook is `Send + Sync`.
unsafe impl Send for LiveInner {}
unsafe impl Sync for LiveInner {}

/// Owning handle to a Virtualization.framework VM.
pub struct VzHost {
    inner: Mutex<Option<LiveInner>>,
}

impl VzHost {
    /// Build the Swift VM for `plan`. `hook` receives mapped VmState ints.
    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all, fields(vm = %plan.name))]
    pub fn create(plan: &VzPlan, hook: StateHook) -> Result<Self, VzError> {
        let boxed = Box::new(hook);
        let ctx = Box::into_raw(boxed) as *mut c_void;
        let handle = match ffi::create(plan, Some(state_trampoline), ctx) {
            Ok(handle) => handle,
            Err(error) => {
                // SAFETY: create failed; reclaim the hook box.
                unsafe {
                    drop(Box::from_raw(ctx as *mut StateHook));
                }
                return Err(error);
            }
        };
        Ok(Self {
            inner: Mutex::new(Some(LiveInner { handle, ctx })),
        })
    }

    fn handle(&self) -> Result<Handle, VzError> {
        lock(&self.inner)
            .as_ref()
            .map(|inner| inner.handle)
            .ok_or(VzError::Host {
                op: "handle",
                detail: "destroyed".into(),
            })
    }

    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all)]
    pub fn start(&self) -> Result<(), VzError> {
        ffi::start(self.handle()?)
    }

    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all)]
    pub fn pause(&self) -> Result<(), VzError> {
        ffi::pause(self.handle()?)
    }

    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all)]
    pub fn resume(&self) -> Result<(), VzError> {
        ffi::resume(self.handle()?)
    }

    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all)]
    pub fn stop(&self, force: bool) -> Result<(), VzError> {
        ffi::stop(self.handle()?, force)
    }

    #[tracing::instrument(level = "trace", target = "ternvale::vz", skip_all)]
    pub fn state(&self) -> i32 {
        self.handle().map(ffi::state).unwrap_or(5)
    }
}

impl Drop for VzHost {
    fn drop(&mut self) {
        if let Some(inner) = lock(&self.inner).take() {
            ffi::destroy(inner.handle);
            // SAFETY: unique owner of the box allocated in create; Swift
            // will not invoke the callback after destroy.
            unsafe {
                drop(Box::from_raw(inner.ctx as *mut StateHook));
            }
        }
    }
}

fn lock(mutex: &Mutex<Option<LiveInner>>) -> std::sync::MutexGuard<'_, Option<LiveInner>> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

extern "C" fn state_trampoline(ctx: *mut c_void, state: i32, detail: *const c_char) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: ctx is the StateHook box leaked in create; Swift keeps it until
    // destroy. The C string is valid only for this call.
    let hook = unsafe { &*(ctx as *const StateHook) };
    let detail = unsafe {
        if detail.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(detail)
                .to_string_lossy()
                .into_owned()
        }
    };
    hook(state, detail);
}
