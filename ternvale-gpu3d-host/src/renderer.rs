//! Host renderer: Venus command execution on MoltenVK / Metal.

use crate::Gpu3dError;

/// Host GPU backend. Implementations must not panic on guest input.
pub trait HostRenderer: Send {
    /// Create a 3D context.
    fn context_create(&mut self, ctx_id: u32, name: &str) -> Result<(), Gpu3dError>;
    /// Destroy a 3D context.
    fn context_destroy(&mut self, ctx_id: u32);
    /// Submit a validated command stream (dword-aligned).
    fn submit(&mut self, ctx_id: u32, payload: &[u8]) -> Result<(), Gpu3dError>;
    /// Optional blob create; default is a no-op.
    fn resource_create_blob(
        &mut self,
        _ctx_id: u32,
        _resource_id: u32,
        _blob_mem: u32,
        _size: u64,
    ) -> Result<(), Gpu3dError> {
        Ok(())
    }
    /// Renderer process / device disappeared.
    fn mark_lost(&mut self);
}

/// No host Vulkan. SUBMIT_3D fails unless tests opt into a no-op.
pub struct StubRenderer {
    lost: bool,
    allow_submit: bool,
}

impl StubRenderer {
    /// Production stub (no rendering).
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all)]
    pub fn new() -> Self {
        tracing::info!(
            target: "ternvale::gpu3d",
            host_gpu_path = "software_stub",
            moltenvk = false,
            metal = false,
            "gpu3d stub renderer (install virglrenderer + MoltenVK for host GPU)"
        );
        Self {
            lost: false,
            allow_submit: false,
        }
    }

    /// Unit tests: accept SUBMIT_3D without a GPU.
    pub fn for_tests() -> Self {
        Self {
            lost: false,
            allow_submit: true,
        }
    }
}

impl Default for StubRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl HostRenderer for StubRenderer {
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self),
        fields(ctx_id)
    )]
    fn context_create(&mut self, ctx_id: u32, name: &str) -> Result<(), Gpu3dError> {
        if self.lost {
            return Err(Gpu3dError::GpuLost);
        }
        tracing::debug!(target: "ternvale::gpu3d", ctx_id, name, "stub context_create");
        Ok(())
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self),
        fields(ctx_id)
    )]
    fn context_destroy(&mut self, ctx_id: u32) {
        tracing::debug!(target: "ternvale::gpu3d", ctx_id, "stub context_destroy");
    }

    #[tracing::instrument(
        level = "trace",
        target = "ternvale::gpu3d",
        skip(self, payload),
        fields(ctx_id, bytes = payload.len())
    )]
    fn submit(&mut self, ctx_id: u32, payload: &[u8]) -> Result<(), Gpu3dError> {
        if self.lost {
            return Err(Gpu3dError::GpuLost);
        }
        if !self.allow_submit {
            return Err(Gpu3dError::Unavailable(
                "venus rendering needs libvirglrenderer (TERNVALE_VIRGL_RENDERER) and MoltenVK",
            ));
        }
        tracing::trace!(
            target: "ternvale::gpu3d",
            ctx_id,
            bytes = payload.len(),
            "stub submit_3d accepted"
        );
        Ok(())
    }

    #[tracing::instrument(level = "error", target = "ternvale::gpu3d", skip(self))]
    fn mark_lost(&mut self) {
        self.lost = true;
        tracing::error!(
            target: "ternvale::gpu3d",
            "stub renderer marked GPU lost"
        );
    }
}
