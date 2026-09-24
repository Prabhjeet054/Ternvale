//! Logger configuration.

use std::path::PathBuf;

use crate::error::LogError;

/// Settings for [`crate::init`].
///
/// `level` is the `EnvFilter` directive used when `TERNVALE_LOG` is unset.
/// The default directive is `info`.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// VM name embedded in the log file name.
    pub vm_name: String,
    /// Fallback `EnvFilter` directive when `TERNVALE_LOG` is unset.
    pub level: String,
    /// Directory that receives `ternvale-<vm>-<timestamp>.log`.
    pub log_dir: PathBuf,
    /// Write the file layer as JSON. Stderr stays human-readable.
    pub json: bool,
}

impl LogConfig {
    /// Config with level `info`, JSON off, and `log_dir` chosen by the caller.
    #[tracing::instrument(level = "debug", target = "ternvale::log", skip_all)]
    pub fn new(vm_name: impl Into<String>, log_dir: PathBuf) -> Self {
        let vm_name = vm_name.into();
        tracing::debug!(target: "ternvale::log", %vm_name, "built log config");
        Self {
            vm_name,
            level: "info".to_string(),
            log_dir,
            json: false,
        }
    }

    /// `~/Library/Logs/Ternvale`, the default directory from the project rules.
    #[tracing::instrument(level = "debug", target = "ternvale::log", skip_all)]
    pub fn default_log_dir() -> Result<PathBuf, LogError> {
        let home = std::env::var("HOME").map_err(|err| match err {
            std::env::VarError::NotPresent => LogError::HomeMissing,
            std::env::VarError::NotUnicode(_) => LogError::HomeNotUnicode,
        })?;
        Ok(PathBuf::from(home).join("Library/Logs/Ternvale"))
    }
}

pub(crate) fn validate_vm_name(vm_name: &str) -> Result<(), LogError> {
    let ok = !vm_name.is_empty()
        && vm_name != "."
        && vm_name != ".."
        && vm_name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if ok {
        tracing::debug!(target: "ternvale::log", vm_name, "accepted vm name");
        Ok(())
    } else {
        tracing::warn!(target: "ternvale::log", vm_name, "rejected vm name");
        Err(LogError::InvalidVmName)
    }
}

pub(crate) fn filter_directive(level: &str) -> Result<String, LogError> {
    if level.is_empty() {
        tracing::error!(target: "ternvale::log", "log level directive is empty");
        return Err(LogError::EmptyLevel);
    }
    match std::env::var("TERNVALE_LOG") {
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => Ok(level.to_string()),
        Err(std::env::VarError::NotUnicode(_)) => Err(LogError::EnvNotUnicode),
    }
}

pub(crate) fn log_file_name(vm_name: &str) -> String {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    format!("ternvale-{vm_name}-{stamp}.log")
}
