//! Config failures and the shared cross-crate error.

use std::path::PathBuf;

use thiserror::Error;

/// A VM config document failed to parse or failed validation.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// TOML text was not a `VmConfig`.
    #[error("parse VM config: {source}")]
    Parse {
        /// Parser error. Unknown fields are reported here and name the field.
        source: toml::de::Error,
    },

    /// A `VmConfig` could not be written as TOML.
    #[error("serialize VM config: {source}")]
    Serialize {
        /// Serializer error.
        source: toml::ser::Error,
    },

    /// The config file could not be read.
    #[error("read VM config {}: {source}", path.display())]
    Read {
        /// Path passed to `VmConfig::from_path`.
        path: PathBuf,
        /// IO error from `read_to_string`.
        source: std::io::Error,
    },

    /// `name` is empty or not safe to embed in a log file name.
    #[error("name must be a non-empty ASCII identifier (letters, digits, '-' or '_')")]
    InvalidName,

    /// `cpus` was outside `1..=16`.
    #[error("cpus must be from 1 to 16, got {cpus}")]
    InvalidCpus {
        /// The rejected value.
        cpus: u32,
    },

    /// `ram_mib` was zero or not a multiple of 16.
    #[error("ram_mib must be a positive multiple of 16, got {ram_mib}")]
    InvalidRam {
        /// The rejected value, in MiB.
        ram_mib: u64,
    },

    /// An input file named by the config is missing.
    #[error("{field} is not an existing file: {}", path.display())]
    MissingFile {
        /// Config field, such as `kernel` or `disks[0].path`.
        field: String,
        /// Path that was not a regular file.
        path: PathBuf,
    },

    /// `serial_log` cannot be created because its parent directory is missing,
    /// or the path points at a directory.
    #[error("serial_log parent is not an existing directory: {}", path.display())]
    SerialLogDir {
        /// The `serial_log` path from the config.
        path: PathBuf,
    },

    /// A NIC entry has an empty backend name.
    #[error("nics[{index}].backend is empty")]
    EmptyNicBackend {
        /// Index into `nics`.
        index: usize,
    },
}

/// Error type other Ternvale crates can return without depending on each other.
///
/// Subsystem crates map their own errors into [`TernvaleError::Subsystem`].
/// Config failures use [`TernvaleError::Config`].
#[derive(Debug, Error)]
pub enum TernvaleError {
    /// VM config parse or validation failed.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// A failure from another crate, labeled with that crate's subsystem.
    #[error("{subsystem}: {message}")]
    Subsystem {
        /// Short subsystem name, such as `hv` or `vmm`.
        subsystem: &'static str,
        /// Already-rendered cause. The source crate owns the typed error.
        message: String,
    },
}

impl TernvaleError {
    /// Wrap a message from another crate.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::config",
        skip(message),
        fields(subsystem)
    )]
    pub fn subsystem(subsystem: &'static str, message: impl Into<String>) -> Self {
        let message = message.into();
        tracing::debug!(
            target: "ternvale::config",
            subsystem,
            %message,
            "wrapped subsystem error"
        );
        Self::Subsystem { subsystem, message }
    }
}
