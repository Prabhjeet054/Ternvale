//! Sandbox / Seatbelt errors.

use std::path::PathBuf;

use thiserror::Error;

/// Could not build a profile, bookmark store, or sandboxed child.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// `sandbox-exec` is missing.
    #[error("sandbox-exec not found at {0}; cannot confine the VMM")]
    NoSandboxExec(PathBuf),
    /// Writing or applying the Seatbelt profile failed.
    #[error("seatbelt profile: {0}")]
    Profile(String),
    /// Bookmark file magic/version/contents.
    #[error("bookmark store {path}: {reason}")]
    Bookmarks {
        /// File that failed.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// Security.framework bookmark call.
    #[error(transparent)]
    Sec(#[from] ternvale_hv::SecError),
    /// Spawning `sandbox-exec` failed.
    #[error("start sandboxed VMM: {0}")]
    Spawn(String),
    /// I/O.
    #[error("{op}: {source}")]
    Io {
        /// Operation name.
        op: &'static str,
        /// OS error.
        #[source]
        source: std::io::Error,
    },
}
