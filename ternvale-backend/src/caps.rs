//! Feature flags advertised by a backend.

use serde::{Deserialize, Serialize};

/// Explicit capability bits. A false flag must surface as
/// [`crate::BackendError::Unsupported`], never a silent no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BackendCapabilities {
    pub guest_linux: bool,
    pub guest_windows: bool,
    pub guest_macos: bool,
    pub tpm: bool,
    pub snapshots_disk: bool,
    pub snapshots_memory: bool,
    pub nested_virt: bool,
    pub rosetta: bool,
    pub gpu_2d: bool,
    pub gpu_3d: bool,
    pub usb_passthrough: bool,
    pub bridged_net: bool,
    pub disk_discard: bool,
    pub balloon: bool,
    pub shared_folders: bool,
    pub custom_display_pipeline: bool,
    pub cpu_profiles: bool,
}

impl BackendCapabilities {
    /// Hypervisor.framework orchestrator (Parts 1–3).
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn native() -> Self {
        Self {
            guest_linux: true,
            guest_windows: true,
            guest_macos: false,
            tpm: true,
            snapshots_disk: true,
            snapshots_memory: true,
            nested_virt: false,
            rosetta: false,
            gpu_2d: true,
            gpu_3d: false,
            usb_passthrough: true,
            bridged_net: true,
            disk_discard: true,
            balloon: true,
            shared_folders: true,
            custom_display_pipeline: false,
            cpu_profiles: false,
        }
    }
}
