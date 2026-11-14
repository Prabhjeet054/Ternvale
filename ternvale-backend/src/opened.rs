//! The result of [`crate::open`]: native orchestrator or VZ host.

use ternvale_config::BackendKind;

use crate::{BackendCapabilities, BackendError, NativeBackend, StopMode, VmBackend, VzBackend};

/// Opened backend. Callers branch on this type (or [`Self::drives_guest`]),
/// never on the string `"vz"`.
pub enum Opened {
    /// Hypervisor.framework orchestrator ([`NativeBackend`]).
    Native(NativeBackend),
    /// Virtualization.framework Linux host ([`VzBackend`]).
    Vz(VzBackend),
}

impl Opened {
    /// VZ (and later macOS guests) own vCPUs and devices; the CLI must not
    /// start a Hypervisor.framework [`ternvale_vmm::Machine`].
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn drives_guest(&self) -> bool {
        matches!(self, Self::Vz(_))
    }

    /// Resolved backend name for the control API.
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn kind(&self) -> BackendKind {
        match self {
            Self::Native(_) => BackendKind::Native,
            Self::Vz(_) => BackendKind::Vz,
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn capabilities(&self) -> BackendCapabilities {
        match self {
            Self::Native(backend) => backend.capabilities(),
            Self::Vz(backend) => backend.capabilities(),
        }
    }

    /// The native orchestrator, if this open is not self-hosted.
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn native(&self) -> Option<&NativeBackend> {
        match self {
            Self::Native(backend) => Some(backend),
            Self::Vz(_) => None,
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn start_blocking(&self) -> Result<(), BackendError> {
        match self {
            Self::Native(backend) => pollster::block_on(backend.start()),
            Self::Vz(backend) => pollster::block_on(backend.start()),
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn pause_blocking(&self) -> Result<(), BackendError> {
        match self {
            Self::Native(backend) => pollster::block_on(backend.pause()),
            Self::Vz(backend) => pollster::block_on(backend.pause()),
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn resume_blocking(&self) -> Result<(), BackendError> {
        match self {
            Self::Native(backend) => pollster::block_on(backend.resume()),
            Self::Vz(backend) => pollster::block_on(backend.resume()),
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn subscribe_state(&self) -> std::sync::mpsc::Receiver<ternvale_vmm::VmState> {
        match self {
            Self::Native(backend) => backend.subscribe_state(),
            Self::Vz(backend) => backend.subscribe_state(),
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn stop_blocking(&self, mode: StopMode) -> Result<(), BackendError> {
        match self {
            Self::Native(backend) => pollster::block_on(backend.stop(mode)),
            Self::Vz(backend) => pollster::block_on(backend.stop(mode)),
        }
    }

    /// Unix directory for `host-<port>.sock` (guest→host vsock, including the agent).
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(dir = %dir.display()))]
    pub fn set_vsock_uds_dir(&self, dir: &std::path::Path) {
        if let Self::Vz(backend) = self {
            backend.set_vsock_uds_dir(dir);
        }
    }

    /// Ask the self-hosted backend to show its native display view.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::backend",
        skip_all,
        fields(show_view)
    )]
    pub fn set_show_view(&self, show_view: bool) {
        if let Self::Vz(backend) = self {
            backend.set_show_view(show_view);
        }
    }

    /// Block until the self-hosted VM is Stopped or Failed.
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn wait_stopped(&self) -> Result<(), BackendError> {
        match self {
            Self::Native(_) => Err(BackendError::Unsupported {
                feature: "self_hosted_run",
            }),
            Self::Vz(backend) => backend.wait_terminal(),
        }
    }
}
