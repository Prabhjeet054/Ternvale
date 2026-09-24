//! Tracing setup, panic hook, and log files for Ternvale.
//!
//! [`init`] installs a stderr layer and a file layer. The file is
//! `ternvale-<vm>-<YYYYMMDD-HHMMSS>.log` under `LogConfig::log_dir`
//! (by default `~/Library/Logs/Ternvale`). `TERNVALE_LOG` selects the
//! `EnvFilter` directive; `LogConfig::level` is the fallback (`info`).

mod config;
mod error;
mod panic;
mod subscriber;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

pub use config::LogConfig;
pub use error::LogError;

use config::{filter_directive, log_file_name, validate_vm_name};
use panic::{install as install_panic_hook, FlushHandle, PanicHookRestore};

/// Serializes [`init`] so the process panic hook is installed by one guard at a time.
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// Flushes the file appender and restores the previous panic hook on drop.
#[must_use = "dropping the guard flushes the log file and restores the panic hook"]
pub struct LogGuard {
    log_path: PathBuf,
    flush: FlushHandle,
    panic_hook: Option<PanicHookRestore>,
    _local: tracing::subscriber::DefaultGuard,
    _init_lock: MutexGuard<'static, ()>,
}

impl std::fmt::Debug for LogGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LogGuard")
            .field("log_path", &self.log_path)
            .finish_non_exhaustive()
    }
}

impl LogGuard {
    /// Path of the log file created for this VM.
    #[tracing::instrument(level = "debug", target = "ternvale::log", skip_all, fields(log_file = %self.log_path.display()))]
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}

impl Drop for LogGuard {
    fn drop(&mut self) {
        if let Some(hook) = self.panic_hook.take() {
            hook.restore();
        }
        self.flush.shutdown();
    }
}

/// Install stderr and file subscribers, the panic hook, and return a flush guard.
///
/// The file layer uses `tracing-appender`. Stderr is always human-readable.
/// `config.json` selects JSON for the file only. A thread-local subscriber is
/// installed for this thread, and the process-wide default is set the first
/// time `init` succeeds. Later calls keep the first process-wide subscriber
/// and override the current thread until the returned guard is dropped.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::log",
    skip_all,
    fields(vm_name = %config.vm_name, json = config.json, log_dir = %config.log_dir.display())
)]
pub fn init(config: LogConfig) -> Result<LogGuard, LogError> {
    let init_lock = match INIT_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(target: "ternvale::log", "log init lock was poisoned; continuing");
            poisoned.into_inner()
        }
    };
    validate_vm_name(&config.vm_name)?;
    let directive = filter_directive(&config.level)?;
    std::fs::create_dir_all(&config.log_dir).map_err(|source| LogError::CreateLogDir {
        path: config.log_dir.clone(),
        source,
    })?;
    let file_name = log_file_name(&config.vm_name);
    let log_path = config.log_dir.join(&file_name);
    let appender = tracing_appender::rolling::never(&config.log_dir, &file_name);
    let (writer, worker) = tracing_appender::non_blocking(appender);
    let built = subscriber::build(config.json, &directive, writer)?;
    let subscriber = std::sync::Arc::new(built);
    let local = tracing::subscriber::set_default(std::sync::Arc::clone(&subscriber));
    if let Err(error) = tracing::subscriber::set_global_default(subscriber) {
        tracing::debug!(
            target: "ternvale::log",
            error = %error,
            "global tracing subscriber already set; this thread uses the new subscriber"
        );
    }
    let flush = FlushHandle::new(worker);
    let panic_hook = install_panic_hook();
    tracing::info!(
        target: "ternvale::log",
        log_file = %log_path.display(),
        %directive,
        json = config.json,
        "logging initialized"
    );
    Ok(LogGuard {
        log_path,
        flush,
        panic_hook: Some(panic_hook),
        _local: local,
        _init_lock: init_lock,
    })
}

/// Log one Hypervisor.framework call on `ternvale::hv`.
///
/// Always emits TRACE with `name`, `args`, and `result_code`. A non-zero code
/// is also logged at ERROR. Returns `code`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::hv",
    skip(args),
    fields(name = %name, result_code = code)
)]
pub fn log_hv_result(name: &str, args: &dyn std::fmt::Debug, code: i32) -> i32 {
    tracing::trace!(
        target: "ternvale::hv",
        name,
        args = ?args,
        result_code = code,
        "hv call completed"
    );
    if code != 0 {
        tracing::error!(
            target: "ternvale::hv",
            name,
            args = ?args,
            result_code = code,
            "hv call failed"
        );
    }
    code
}

/// Log one Hypervisor.framework call: TRACE always, ERROR when `code` is non-zero.
///
/// `code` is an `i32` (`hv_return_t` values should be passed as `i32`).
#[macro_export]
macro_rules! log_hv_call {
    ($name:expr, $args:expr, $code:expr) => {
        $crate::log_hv_result($name, &$args, $code)
    };
}

#[cfg(test)]
mod tests {
    use super::{init, LogConfig};

    use std::sync::{Mutex, OnceLock};

    fn test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// Saves and restores an environment variable.
    ///
    /// Tests hold [`test_lock`] for the whole critical section so no other test
    /// reads `TERNVALE_LOG` while it is temporarily changed.
    struct EnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: `test_lock` is held by the caller, so no other Ternvale test
            // reads or writes this variable until the guard drops.
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: same as `EnvVar::set`: the test lock serializes env access.
            unsafe { std::env::remove_var(key) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            // SAFETY: still inside the test that holds `test_lock`, or dropping
            // at the end of that critical section before the lock is released.
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn temp_dir() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("ternvale-log-{}-{}", std::process::id(), nanos))
    }

    fn read_log(guard: super::LogGuard) -> String {
        let path = guard.log_path().to_path_buf();
        drop(guard);
        std::fs::read_to_string(path).expect("log file")
    }

    fn save_sample(name: &str, text: &str) {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ternvale-log-samples");
        std::fs::create_dir_all(&dir).expect("sample dir");
        std::fs::write(dir.join(name), text).expect("sample log");
    }

    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-log");
    }

    #[test]
    fn default_log_dir_uses_home() {
        let dir = LogConfig::default_log_dir().expect("HOME");
        assert!(dir.ends_with("Library/Logs/Ternvale"));
    }

    #[test]
    fn rejects_vm_name_with_path_separator() {
        let _lock = test_lock().lock().expect("test lock");
        let _env = EnvVar::unset("TERNVALE_LOG");
        let error = init(LogConfig::new("../escape", temp_dir())).unwrap_err();
        assert!(error.to_string().contains("vm name"));
    }

    #[test]
    fn init_writes_an_info_line() {
        let _lock = test_lock().lock().expect("test lock");
        let _env = EnvVar::unset("TERNVALE_LOG");
        let dir = temp_dir();
        let guard = init(LogConfig::new("samplevm", dir.clone())).expect("init");
        let path = guard.log_path().to_path_buf();
        assert!(path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with("ternvale-samplevm-") && name.ends_with(".log")
            }));
        tracing::info!(target: "ternvale::cli", "vm start");
        let text = read_log(guard);
        assert!(text.contains("vm start"), "{text}");
        assert!(text.contains("INFO"), "{text}");
        assert!(text.contains("ternvale::cli"), "{text}");
        assert!(text.contains("ThreadId"), "{text}");
        save_sample("info.log", &text);
        std::fs::remove_dir_all(dir).expect("remove temp log dir");
    }

    #[test]
    fn ternvale_log_env_filters_targets() {
        let _lock = test_lock().lock().expect("test lock");
        let _env = EnvVar::set("TERNVALE_LOG", "ternvale::mmio=trace,info");
        let guard = init(LogConfig::new("filtervm", temp_dir())).expect("init");
        tracing::trace!(target: "ternvale::mmio", "mmio-trace-kept");
        tracing::debug!(target: "ternvale::mmio", "mmio-debug-kept");
        tracing::trace!(target: "ternvale::hv", "hv-trace-dropped");
        tracing::debug!(target: "ternvale::hv", "hv-debug-dropped");
        tracing::info!(target: "ternvale::hv", "hv-info-kept");
        let text = read_log(guard);
        assert!(text.contains("mmio-trace-kept"), "{text}");
        assert!(text.contains("mmio-debug-kept"), "{text}");
        assert!(text.contains("hv-info-kept"), "{text}");
        assert!(!text.contains("hv-trace-dropped"), "{text}");
        assert!(!text.contains("hv-debug-dropped"), "{text}");
        save_sample("filter.log", &text);
    }

    #[test]
    fn panic_hook_writes_message_location_and_backtrace() {
        let _lock = test_lock().lock().expect("test lock");
        let _env = EnvVar::unset("TERNVALE_LOG");
        let guard = init(LogConfig::new("panicvm", temp_dir())).expect("init");
        let panicked = std::panic::catch_unwind(|| {
            panic!("ternvale-log-panic-marker");
        });
        assert!(panicked.is_err());
        let text = read_log(guard);
        assert!(text.contains("ternvale-log-panic-marker"), "{text}");
        assert!(text.contains("ERROR"), "{text}");
        assert!(text.contains("ternvale::log"), "{text}");
        assert!(text.contains("lib.rs"), "{text}");
        assert!(
            text.contains("ternvale_log::") || text.contains("backtrace"),
            "{text}"
        );
        save_sample("panic.log", &text);
    }

    #[test]
    fn log_hv_call_logs_error_on_nonzero_and_trace_when_enabled() {
        let _lock = test_lock().lock().expect("test lock");
        let _env = EnvVar::set("TERNVALE_LOG", "trace");
        let guard = init(LogConfig::new("hvvm", temp_dir())).expect("init");
        let failed = log_hv_call!("hv_vm_create", "flags=0", 1);
        let ok = log_hv_call!("hv_vm_destroy", "none", 0);
        assert_eq!(failed, 1);
        assert_eq!(ok, 0);
        let text = read_log(guard);
        assert!(text.contains("hv call failed"), "{text}");
        assert!(text.contains("hv_vm_create"), "{text}");
        assert!(text.contains("hv call completed"), "{text}");
        let destroy_errors = text
            .lines()
            .filter(|line| line.contains("hv_vm_destroy") && line.contains("ERROR"))
            .count();
        assert_eq!(destroy_errors, 0, "{text}");
        save_sample("hv-call.log", &text);
    }

    #[test]
    fn json_file_layer_emits_json() {
        let _lock = test_lock().lock().expect("test lock");
        let _env = EnvVar::unset("TERNVALE_LOG");
        let mut config = LogConfig::new("jsonvm", temp_dir());
        config.json = true;
        let guard = init(config).expect("init");
        tracing::info!(target: "ternvale::boot", "boot milestone");
        let text = read_log(guard);
        assert!(text.contains("\"level\":\"INFO\""), "{text}");
        assert!(text.contains("boot milestone"), "{text}");
        assert!(text.contains("ternvale::boot"), "{text}");
    }
}
