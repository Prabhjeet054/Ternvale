//! Process panic hook that records message, location, and backtrace.

use std::panic::PanicHookInfo;
use std::sync::{Arc, Mutex};

use tracing_appender::non_blocking::WorkerGuard;

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync>;

/// Owns the appender thread. Dropping the inner guard flushes the log file.
#[derive(Clone)]
pub(crate) struct FlushHandle {
    inner: Arc<Mutex<Option<WorkerGuard>>>,
}

impl FlushHandle {
    pub(crate) fn new(guard: WorkerGuard) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(guard))),
        }
    }

    /// Drop the worker so its thread joins and buffered lines hit disk.
    ///
    /// `WorkerGuard` has no public flush. Dropping it sends shutdown and waits
    /// for the appender thread, which is the flush-on-drop behavior.
    pub(crate) fn shutdown(&self) {
        match self.inner.lock() {
            Ok(mut slot) => {
                slot.take();
            }
            Err(poisoned) => {
                poisoned.into_inner().take();
            }
        }
    }
}

pub(crate) struct PanicHookRestore {
    previous: Arc<Mutex<Option<PanicHook>>>,
}

pub(crate) fn install() -> PanicHookRestore {
    let previous = Arc::new(Mutex::new(Some(std::panic::take_hook())));
    let previous_for_hook = Arc::clone(&previous);
    std::panic::set_hook(Box::new(move |info| {
        log_panic(info);
        call_previous(&previous_for_hook, info);
    }));
    PanicHookRestore { previous }
}

impl PanicHookRestore {
    pub(crate) fn restore(self) {
        let previous = match self.previous.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(hook) = previous {
            std::panic::set_hook(hook);
        }
    }
}

fn call_previous(previous: &Arc<Mutex<Option<PanicHook>>>, info: &PanicHookInfo<'_>) {
    let hook = match previous.lock() {
        Ok(mut slot) => slot.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    };
    let Some(hook) = hook else {
        return;
    };
    hook(info);
    match previous.lock() {
        Ok(mut slot) => *slot = Some(hook),
        Err(poisoned) => *poisoned.into_inner() = Some(hook),
    }
}

fn log_panic(info: &PanicHookInfo<'_>) {
    let message = panic_message(info);
    let location = info.location().map_or_else(
        || "unknown".to_string(),
        |loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()),
    );
    let backtrace = std::backtrace::Backtrace::force_capture();
    tracing::error!(
        target: "ternvale::log",
        %message,
        %location,
        %backtrace,
        "panic"
    );
}

fn panic_message(info: &PanicHookInfo<'_>) -> String {
    if let Some(message) = info.payload().downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    }
}
