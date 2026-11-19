//! `[gpu]`: virtio-gpu 3D (Venus) opt-in.

use serde::{Deserialize, Serialize};

use crate::vm::VmConfig;

/// The `[gpu]` table. Omitted: 2D scanout only (`gpu3d = false`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct GpuSection {
    /// Host Venus renderer (virtio-gpu blobs/contexts/`SUBMIT_3D`). Default false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gpu3d: bool,
}

impl VmConfig {
    /// `[gpu]`, or defaults (3D off) when the table is absent.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name))]
    pub fn gpu_section(&self) -> GpuSection {
        self.gpu.clone().unwrap_or_default()
    }

    /// Per-VM 3D flag. Default **false** even when the VMM is built with `gpu-3d`.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name))]
    pub fn gpu3d(&self) -> bool {
        self.gpu.as_ref().is_some_and(|gpu| gpu.gpu3d)
    }
}

#[cfg(test)]
mod tests {
    use super::GpuSection;

    #[test]
    fn gpu3d_defaults_false() {
        assert!(!GpuSection::default().gpu3d);
    }
}
