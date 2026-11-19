//! Errors for untrusted guest 3D command handling.

/// Why a 3D request was rejected. Never panic on guest input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Gpu3dError {
    /// Command stream is truncated or misaligned.
    #[error("gpu3d decode: {0}")]
    Decode(&'static str),
    /// Command count or payload exceeds the host cap.
    #[error("gpu3d limit: {0}")]
    Limit(&'static str),
    /// Context, resource, or fence id is unknown or reserved.
    #[error("gpu3d object id {id} is invalid ({kind})")]
    BadId {
        /// Guest-supplied identifier.
        id: u64,
        /// What the id was supposed to name.
        kind: &'static str,
    },
    /// Blob size, offset, or alignment is not acceptable.
    #[error("gpu3d mapping: {0}")]
    Mapping(&'static str),
    /// Host renderer reported the GPU as lost.
    #[error("gpu3d host GPU lost")]
    GpuLost,
    /// Renderer is not available (no virglrenderer / MoltenVK).
    #[error("gpu3d renderer unavailable: {0}")]
    Unavailable(&'static str),
    /// Renderer returned a non-zero status.
    #[error("gpu3d renderer failed: {0}")]
    Renderer(i32),
}

impl Gpu3dError {
    /// virtio-gpu response type for this failure.
    #[tracing::instrument(level = "trace", target = "ternvale::gpu3d", skip_all)]
    pub fn virtio_status(&self) -> u32 {
        match self {
            Self::BadId {
                kind: "resource", ..
            } => 0x1203, // RESP_ERR_INVALID_RESOURCE_ID
            Self::Decode(_) | Self::Limit(_) | Self::BadId { .. } | Self::Mapping(_) => 0x1205,
            Self::GpuLost | Self::Unavailable(_) | Self::Renderer(_) => 0x1200,
        }
    }
}
