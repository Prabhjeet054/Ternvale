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
    /// FEX-Emu in a Linux guest (native; also allowed on VZ Linux).
    #[serde(default)]
    pub fex: bool,
    /// Distro qemu-user-static (not bundled).
    #[serde(default)]
    pub qemu_user: bool,
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
            fex: true,
            qemu_user: true,
            gpu_2d: true,
            gpu_3d: false,
            usb_passthrough: true,
            bridged_net: true,
            disk_discard: true,
            balloon: true,
            shared_folders: true,
            // virtio-gpu presenter + app Metal viewer (TVDP). Not VZVirtualMachineView.
            custom_display_pipeline: true,
            cpu_profiles: false,
        }
    }

    /// Virtualization.framework Linux and macOS guests (Steps 92–93).
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn linux_vz() -> Self {
        Self {
            guest_linux: true,
            guest_windows: false,
            guest_macos: false,
            tpm: false,
            snapshots_disk: false,
            // VZ `saveMachineStateTo` (macOS 14+), not the native RAM/GIC bundle.
            snapshots_memory: true,
            nested_virt: false,
            rosetta: true,
            fex: true,
            qemu_user: true,
            gpu_2d: false,
            gpu_3d: false,
            usb_passthrough: false,
            bridged_net: false,
            disk_discard: false,
            balloon: false,
            shared_folders: false,
            custom_display_pipeline: false,
            cpu_profiles: false,
        }
    }

    /// Virtualization.framework macOS guest (Step 93). Display pipeline is VZ's
    /// (Step 94); `custom_display_pipeline` stays false.
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn macos_vz() -> Self {
        Self {
            guest_linux: false,
            guest_windows: false,
            guest_macos: true,
            tpm: false,
            snapshots_disk: false,
            snapshots_memory: true,
            nested_virt: false,
            rosetta: false,
            fex: false,
            qemu_user: false,
            gpu_2d: false,
            gpu_3d: false,
            usb_passthrough: false,
            bridged_net: false,
            disk_discard: false,
            balloon: false,
            shared_folders: true,
            custom_display_pipeline: false,
            cpu_profiles: false,
        }
    }

    /// x86 usermode bits for [`ternvale_compat::resolve`].
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn x86(&self) -> ternvale_compat::EngineCaps {
        ternvale_compat::EngineCaps {
            rosetta: self.rosetta,
            fex: self.fex,
            qemu_user: self.qemu_user,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BackendCapabilities;

    #[test]
    fn macos_vz_uses_vz_display_and_shares() {
        let caps = BackendCapabilities::macos_vz();
        assert!(caps.guest_macos);
        assert!(caps.shared_folders);
        assert!(!caps.custom_display_pipeline);
        assert!(caps.snapshots_memory);
        assert!(!caps.snapshots_disk);
    }

    #[test]
    fn native_linux_keeps_metal_display_pipeline() {
        let caps = BackendCapabilities::native();
        assert!(caps.guest_linux);
        assert!(caps.gpu_2d);
        assert!(caps.custom_display_pipeline);
        assert!(!caps.guest_macos);
        assert!(!caps.rosetta);
        assert!(caps.fex);
        assert!(caps.qemu_user);
    }

    #[test]
    fn linux_vz_advertises_rosetta_only() {
        let linux = BackendCapabilities::linux_vz();
        assert!(linux.rosetta);
        assert!(linux.fex);
        assert!(linux.guest_linux);
        assert!(!BackendCapabilities::macos_vz().rosetta);
        assert!(!BackendCapabilities::macos_vz().fex);
        assert!(!BackendCapabilities::native().rosetta);
        assert!(BackendCapabilities::native().fex);
    }
}
