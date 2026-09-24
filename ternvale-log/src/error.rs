//! Errors from installing the Ternvale logger.

use std::path::PathBuf;

use thiserror::Error;

/// Failure while building or installing the global logger.
#[derive(Debug, Error)]
pub enum LogError {
    /// `vm_name` would escape the log directory or produce an ambiguous file name.
    #[error("vm name is empty or contains a path separator")]
    InvalidVmName,

    /// `LogConfig::level` was empty. The fallback directive must be non-empty.
    #[error("log level directive is empty")]
    EmptyLevel,

    /// `HOME` is unset, so the default log directory cannot be built.
    #[error("HOME is not set; cannot resolve ~/Library/Logs/Ternvale")]
    HomeMissing,

    /// `HOME` was present but not Unicode.
    #[error("HOME is not valid unicode")]
    HomeNotUnicode,

    /// `TERNVALE_LOG` was present but not Unicode.
    #[error("TERNVALE_LOG is not valid unicode")]
    EnvNotUnicode,

    /// `TERNVALE_LOG` or `LogConfig::level` was not a valid `EnvFilter` directive.
    #[error("invalid log filter directive {directive}: {source}")]
    InvalidFilter {
        /// The directive that failed to parse.
        directive: String,
        /// Parse error from `tracing-subscriber`.
        source: tracing_subscriber::filter::ParseError,
    },

    /// `create_dir_all` failed for the log directory.
    #[error("create log directory {}: {source}", path.display())]
    CreateLogDir {
        /// Directory we tried to create.
        path: PathBuf,
        /// IO error from `create_dir_all`.
        source: std::io::Error,
    },
}
