//! Errors from a [`crate::VmBackend`].

use ternvale_vmm::ControlError;

/// A backend request failed.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// The operation is not valid in the current VM state.
    #[error("cannot {op} a vm that is {state}")]
    InvalidTransition {
        /// Requested operation.
        op: &'static str,
        /// Current state name.
        state: String,
    },
    /// The backend (or this build) does not implement `feature`.
    #[error("not supported by this backend: {feature}")]
    Unsupported {
        /// Capability or backend name, such as `vz` or `nested_virt`.
        feature: &'static str,
    },
    /// A config option this backend cannot honour.
    #[error("{option} is not supported by this backend (capability {capability})")]
    UnsupportedOption {
        /// TOML / config field.
        option: String,
        /// [`crate::BackendCapabilities`] flag name, or `none`.
        capability: &'static str,
    },
    /// Orchestrator [`ControlError`].
    #[error(transparent)]
    Control(#[from] ControlError),
    /// Host I/O (agent socket, media path).
    #[error("{op}: {detail}")]
    Io {
        /// Operation name.
        op: &'static str,
        /// Display text (no secrets).
        detail: String,
    },
}

impl BackendError {
    /// Map a lifecycle [`ControlError`] to [`BackendError`].
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn from_control(error: ControlError) -> Self {
        match error {
            ControlError::InvalidState { op, state, .. } => {
                tracing::warn!(
                    target: "ternvale::backend",
                    op,
                    state = %state,
                    "invalid backend transition"
                );
                Self::InvalidTransition {
                    op,
                    state: state.as_str().to_string(),
                }
            }
            ControlError::SnapshotHostMissing => {
                tracing::warn!(
                    target: "ternvale::backend",
                    "snapshot host missing; mapping to unsupported"
                );
                Self::Unsupported {
                    feature: "snapshots",
                }
            }
            other => Self::Control(other),
        }
    }
}
