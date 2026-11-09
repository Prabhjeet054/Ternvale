//! Host sleep/wake (IOKit) and SIGINT/SIGTERM/SIGUSR1/SIGUSR2 (dispatch SIGNAL).
//! SIGUSR1/SIGUSR2 inject WillSleep/DidWake so tests can exercise pause +
//! time_sync without putting the Mac to sleep.
//!
//! Raw FFI stays in this crate. Sleep uses `IORegisterForSystemPower`
//! (IOPMLib); quit uses `DISPATCH_SOURCE_TYPE_SIGNAL` after `SIG_IGN` so the
//! default terminate action does not kill the process before the callback
//! runs. TODO(verify): IOKit message constants vs current SDK `IOMessage.h`.

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// What the host just did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPowerEvent {
    /// `kIOMessageSystemWillSleep`: park vCPUs before the Mac sleeps.
    WillSleep,
    /// `kIOMessageSystemHasPoweredOn`: unpark and resync guest time.
    DidWake,
    /// SIGINT or SIGTERM: apply `[power] on_quit`.
    QuitRequested,
}

const SIGINT: usize = 2;
const SIGTERM: usize = 15;
/// Darwin `SIGUSR1`: inject `WillSleep` without putting the Mac to sleep.
const SIGUSR1: usize = 30;
/// Darwin `SIGUSR2`: inject `DidWake`.
const SIGUSR2: usize = 31;
const SIG_IGN: usize = 1;
const KIO_MESSAGE_SYSTEM_WILL_SLEEP: u32 = 0xe000_0280;
const KIO_MESSAGE_CAN_SYSTEM_SLEEP: u32 = 0xe000_0270;
const KIO_MESSAGE_SYSTEM_HAS_POWERED_ON: u32 = 0xe000_0320;

type IoConnect = u32;
type IoObject = u32;

struct SleepState {
    callback: Arc<dyn Fn(HostPowerEvent) + Send + Sync>,
    root_port: IoConnect,
}

struct SignalState {
    callback: Arc<dyn Fn(HostPowerEvent) + Send + Sync>,
    event: HostPowerEvent,
    source: AtomicPtr<c_void>,
}

/// Live host-power watches. Drop cancels them.
pub struct HostPowerWatch {
    sleep: Option<SleepWatch>,
    signals: Vec<SignalWatch>,
}

struct SleepWatch {
    runloop: Arc<Mutex<usize>>,
    thread: Option<JoinHandle<()>>,
    state: *mut SleepState,
}

struct SignalWatch {
    source: *mut c_void,
    ctx: *mut SignalState,
}

// SAFETY: IOKit / dispatch objects are process-wide; callbacks are `Send`.
unsafe impl Send for HostPowerWatch {}
unsafe impl Sync for HostPowerWatch {}
unsafe impl Send for SleepWatch {}
unsafe impl Sync for SleepWatch {}
unsafe impl Send for SignalWatch {}
unsafe impl Sync for SignalWatch {}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegisterForSystemPower(
        refcon: *mut c_void,
        the_port_ref: *mut *mut c_void,
        callback: Option<unsafe extern "C" fn(*mut c_void, u32, u32, *mut c_void)>,
        notifier: *mut IoObject,
    ) -> IoConnect;
    fn IOAllowPowerChange(kernel_port: IoConnect, notification_id: isize) -> i32;
    fn IODeregisterForSystemPower(notifier: *mut IoObject) -> i32;
    fn IONotificationPortGetRunLoopSource(port: *mut c_void) -> *mut c_void;
    fn IONotificationPortDestroy(port: *mut c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: *mut c_void);
    fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: *const c_void);
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    static _dispatch_source_type_signal: u8;
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
    fn signal(sig: i32, handler: usize) -> usize;
}

/// Subscribe to Mac sleep/wake and to SIGINT/SIGTERM.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all)]
pub fn watch_host_power(
    callback: Arc<dyn Fn(HostPowerEvent) + Send + Sync>,
) -> Option<HostPowerWatch> {
    let sleep = watch_sleep(Arc::clone(&callback));
    let mut signals = Vec::new();
    for (sig, event) in [
        (SIGINT, HostPowerEvent::QuitRequested),
        (SIGTERM, HostPowerEvent::QuitRequested),
        (SIGUSR1, HostPowerEvent::WillSleep),
        (SIGUSR2, HostPowerEvent::DidWake),
    ] {
        if let Some(watch) = watch_signal(sig, event, Arc::clone(&callback)) {
            signals.push(watch);
        }
    }
    if sleep.is_none() && signals.is_empty() {
        tracing::warn!(target: "ternvale::hv", "host power watch unavailable");
        return None;
    }
    tracing::info!(
        target: "ternvale::hv",
        sleep = sleep.is_some(),
        signals = signals.len(),
        "watching host sleep/wake, quit, and SIGUSR1/SIGUSR2 inject"
    );
    Some(HostPowerWatch { sleep, signals })
}

fn watch_sleep(callback: Arc<dyn Fn(HostPowerEvent) + Send + Sync>) -> Option<SleepWatch> {
    let state = Box::into_raw(Box::new(SleepState {
        callback,
        root_port: 0,
    }));
    let runloop = Arc::new(Mutex::new(0usize));
    let loop_slot = Arc::clone(&runloop);
    let state_addr = state as usize;
    // SAFETY: `state` is owned until Drop; the IOKit callback only runs while
    // the notification port's run loop is alive. Addresses are sent to the
    // sleep thread as usize because raw pointers are not `Send`.
    let thread = std::thread::Builder::new()
        .name("ternvale-host-sleep".into())
        .spawn(move || sleep_thread(state_addr as *mut SleepState, loop_slot))
        .ok()?;
    Some(SleepWatch {
        runloop,
        thread: Some(thread),
        state,
    })
}

fn sleep_thread(state: *mut SleepState, runloop: Arc<Mutex<usize>>) {
    let mut notify_port: *mut c_void = std::ptr::null_mut();
    let mut notifier: IoObject = 0;
    // SAFETY: IORegisterForSystemPower writes the port and notifier; a zero
    // return means registration failed.
    let root = unsafe {
        IORegisterForSystemPower(
            state.cast(),
            &mut notify_port,
            Some(sleep_cb),
            &mut notifier,
        )
    };
    if root == 0 || notify_port.is_null() {
        tracing::warn!(target: "ternvale::hv", "IORegisterForSystemPower failed");
        return;
    }
    // SAFETY: `state` is the Box allocated for this sleep thread.
    unsafe {
        (*state).root_port = root;
    }
    // SAFETY: `notify_port` is the IONotificationPort from registration.
    let source = unsafe { IONotificationPortGetRunLoopSource(notify_port) };
    // SAFETY: this is the sleep thread; CFRunLoopGetCurrent is this thread's loop.
    let rl = unsafe { CFRunLoopGetCurrent() };
    if let Ok(mut slot) = runloop.lock() {
        *slot = rl as usize;
    }
    // SAFETY: `notify_port` and `source` stay live until `CFRunLoopRun` returns.
    unsafe {
        CFRunLoopAddSource(rl, source, kCFRunLoopDefaultMode);
        CFRunLoopRun();
        IODeregisterForSystemPower(&mut notifier);
        IONotificationPortDestroy(notify_port);
    }
}

unsafe extern "C" fn sleep_cb(
    refcon: *mut c_void,
    _service: u32,
    message_type: u32,
    message_argument: *mut c_void,
) {
    if refcon.is_null() {
        return;
    }
    let state = refcon.cast::<SleepState>();
    // SAFETY: IOKit holds `refcon` until we deregister; it is our SleepState Box.
    let root = unsafe { (*state).root_port };
    match message_type {
        KIO_MESSAGE_CAN_SYSTEM_SLEEP | KIO_MESSAGE_SYSTEM_WILL_SLEEP => {
            if message_type == KIO_MESSAGE_SYSTEM_WILL_SLEEP {
                tracing::info!(target: "ternvale::hv", "host will sleep");
                // SAFETY: callback lives in SleepState until Drop after unregister.
                (unsafe { &(*state).callback })(HostPowerEvent::WillSleep);
            }
            // SAFETY: IOPMLib requires IOAllowPowerChange with the token from
            // this callback so the Mac can finish sleeping.
            let _ = unsafe { IOAllowPowerChange(root, message_argument as isize) };
        }
        KIO_MESSAGE_SYSTEM_HAS_POWERED_ON => {
            tracing::info!(target: "ternvale::hv", "host did wake");
            // SAFETY: same SleepState as WillSleep; still registered.
            (unsafe { &(*state).callback })(HostPowerEvent::DidWake);
        }
        _ => {}
    }
}

fn watch_signal(
    sig: usize,
    event: HostPowerEvent,
    callback: Arc<dyn Fn(HostPowerEvent) + Send + Sync>,
) -> Option<SignalWatch> {
    // SAFETY: SIG_IGN replaces the default terminate action; dispatch then
    // delivers the signal to our handler.
    unsafe {
        signal(sig as i32, SIG_IGN);
    }
    let ctx = Box::into_raw(Box::new(SignalState {
        callback,
        event,
        source: AtomicPtr::new(std::ptr::null_mut()),
    }));
    // SAFETY: the global concurrent queue outlives this process; the source
    // is owned by SignalWatch after a non-null return.
    let source = unsafe {
        let queue = dispatch_get_global_queue(0, 0);
        let kind = std::ptr::addr_of!(_dispatch_source_type_signal).cast::<c_void>();
        dispatch_source_create(kind, sig, 0, queue)
    };
    if source.is_null() {
        // SAFETY: create failed; `ctx` is the Box we just allocated and never published.
        unsafe {
            drop(Box::from_raw(ctx));
        }
        tracing::warn!(target: "ternvale::hv", sig, "dispatch SIGNAL source was null");
        return None;
    }
    // SAFETY: `ctx` is still uniquely ours; `source` is a live dispatch object.
    unsafe {
        (*ctx).source.store(source, Ordering::Release);
        dispatch_set_context(source, ctx.cast());
        dispatch_source_set_event_handler_f(source, Some(signal_event));
        dispatch_resume(source);
    }
    Some(SignalWatch { source, ctx })
}

unsafe extern "C" fn signal_event(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    let state = ctx.cast::<SignalState>();
    // SAFETY: dispatch context is SignalState until we cancel the source in Drop.
    let event = unsafe { (*state).event };
    tracing::info!(target: "ternvale::hv", ?event, "host power signal");
    (unsafe { &(*state).callback })(event);
}

impl Drop for HostPowerWatch {
    fn drop(&mut self) {
        self.sleep.take();
        self.signals.clear();
    }
}

impl Drop for SleepWatch {
    fn drop(&mut self) {
        if let Ok(rl) = self.runloop.lock() {
            if *rl != 0 {
                // SAFETY: `rl` is the sleep thread's run loop.
                unsafe { CFRunLoopStop(*rl as *mut c_void) };
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if !self.state.is_null() {
            // SAFETY: unique Box from watch start; Drop runs once after the thread joins.
            unsafe {
                drop(Box::from_raw(self.state));
            }
            self.state = std::ptr::null_mut();
        }
    }
}

impl Drop for SignalWatch {
    fn drop(&mut self) {
        if !self.source.is_null() {
            // SAFETY: `source` came from dispatch_source_create in watch_signal.
            unsafe {
                dispatch_source_cancel(self.source);
                dispatch_release(self.source);
            }
            self.source = std::ptr::null_mut();
        }
        if !self.ctx.is_null() {
            // SAFETY: unique Box; cancel already issued so the handler will not run.
            unsafe {
                drop(Box::from_raw(self.ctx));
            }
            self.ctx = std::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HostPowerEvent;

    #[test]
    fn events_have_stable_names() {
        assert_ne!(HostPowerEvent::WillSleep, HostPowerEvent::DidWake);
        assert_ne!(HostPowerEvent::QuitRequested, HostPowerEvent::DidWake);
    }
}
