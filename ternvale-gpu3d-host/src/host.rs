//! Combined host 3D device: validate, map, fence, then render.

use crate::decoder::decode_submit;
use crate::mapping::{GuestMapper, HostVisiblePool, RecordingMapper};
use crate::renderer::{HostRenderer, StubRenderer};
use crate::state::Gpu3dState;
use crate::Gpu3dError;

/// virtio-gpu 3D host path (Venus → renderer → MoltenVK).
pub struct HostGpu {
    state: Gpu3dState,
    pool: HostVisiblePool,
    mapper: Box<dyn GuestMapper>,
    renderer: Box<dyn HostRenderer>,
}

impl HostGpu {
    /// Production path: recording mapper until the VMM installs [`crate::HvMapper`].
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all)]
    pub fn new() -> Self {
        let renderer = open_renderer();
        Self {
            state: Gpu3dState::new(),
            pool: HostVisiblePool::new(),
            mapper: Box::new(RecordingMapper::default()),
            renderer,
        }
    }

    /// Tests: stub renderer that accepts submits; no HV maps.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all)]
    pub fn for_tests() -> Self {
        Self {
            state: Gpu3dState::new(),
            pool: HostVisiblePool::new(),
            mapper: Box::new(RecordingMapper::default()),
            renderer: Box::new(StubRenderer::for_tests()),
        }
    }

    /// Use `hv_vm_map` for host-visible blobs (call after the VM exists).
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all)]
    pub fn with_hv_mapper(mut self) -> Self {
        self.mapper = Box::new(crate::HvMapper);
        self
    }

    /// Device reset.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip(self))]
    pub fn reset(&mut self) {
        self.state.reset();
    }

    /// `CTX_CREATE` with Venus `context_init`.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip(self, name))]
    pub fn ctx_create(
        &mut self,
        ctx_id: u32,
        context_init: u32,
        name: &[u8],
    ) -> Result<(), Gpu3dError> {
        self.state.ctx_create(ctx_id, context_init, name)?;
        let label = String::from_utf8_lossy(name);
        if let Err(error) = self.renderer.context_create(ctx_id, label.as_ref()) {
            self.state.ctx_destroy(ctx_id);
            return Err(error);
        }
        Ok(())
    }

    /// `CTX_DESTROY`.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip(self))]
    pub fn ctx_destroy(&mut self, ctx_id: u32) {
        self.renderer.context_destroy(ctx_id);
        self.state.ctx_destroy(ctx_id);
    }

    /// Blob create (metadata + renderer).
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip(self))]
    pub fn create_blob(
        &mut self,
        ctx_id: u32,
        resource_id: u32,
        blob_mem: u32,
        size: u64,
    ) -> Result<(), Gpu3dError> {
        self.state.create_blob(resource_id, blob_mem, size)?;
        self.renderer
            .resource_create_blob(ctx_id, resource_id, blob_mem, size)
    }

    /// Attach/detach blob on a context.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip(self))]
    pub fn ctx_resource(
        &mut self,
        ctx_id: u32,
        resource_id: u32,
        attach: bool,
    ) -> Result<(), Gpu3dError> {
        self.state.ctx_resource(ctx_id, resource_id, attach)
    }

    /// Map a HOST3D blob into the PCI BAR window.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip(self))]
    pub fn map_blob(&mut self, resource_id: u32, offset: u64) -> Result<u64, Gpu3dError> {
        let blob = self.state.blob(resource_id)?;
        if offset >= blob.size {
            return Err(Gpu3dError::Mapping("map offset past blob size"));
        }
        self.pool
            .map_blob(self.mapper.as_mut(), resource_id, blob.size, offset)
    }

    /// Unmap a blob.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip(self))]
    pub fn unmap_blob(&mut self, resource_id: u32) -> Result<(), Gpu3dError> {
        self.pool.unmap_blob(self.mapper.as_mut(), resource_id)
    }

    /// Decode and submit. `fence_id` of 0 skips the fence table.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self, payload),
        fields(ctx_id, bytes = payload.len(), fence_id)
    )]
    pub fn submit(&mut self, ctx_id: u32, payload: &[u8], fence_id: u64) -> Result<(), Gpu3dError> {
        if self.state.is_lost() {
            return Err(Gpu3dError::GpuLost);
        }
        self.state.require_ctx(ctx_id)?;
        let cmds = decode_submit(payload)?;
        tracing::trace!(
            target: "ternvale::gpu3d",
            ctx_id,
            commands = cmds.len(),
            "submit_3d decoded"
        );
        match self.renderer.submit(ctx_id, payload) {
            Ok(()) => {
                if fence_id != 0 {
                    self.state.fences.insert_pending(fence_id)?;
                    self.state.fences.signal(fence_id);
                }
                Ok(())
            }
            Err(Gpu3dError::GpuLost) | Err(Gpu3dError::Unavailable(_)) => {
                self.mark_lost();
                Err(Gpu3dError::GpuLost)
            }
            Err(error) => Err(error),
        }
    }

    /// Host renderer died (killed render-server / MoltenVK device lost).
    /// The VMM process must not unwind; later submits return [`Gpu3dError::GpuLost`].
    #[tracing::instrument(level = "error", target = "ternvale::gpu3d", skip(self))]
    pub fn mark_lost(&mut self) {
        tracing::error!(
            target: "ternvale::gpu3d",
            "host renderer process gone; marking GPU lost (VM continues)"
        );
        self.renderer.mark_lost();
        self.state.mark_lost();
    }

    /// GPU-lost flag for the device model.
    pub fn is_lost(&self) -> bool {
        self.state.is_lost()
    }
}

impl Default for HostGpu {
    fn default() -> Self {
        Self::new()
    }
}

fn open_renderer() -> Box<dyn HostRenderer> {
    #[cfg(feature = "virgl")]
    {
        if let Some(virgl) = crate::virgl::VirglRenderer::connect() {
            return Box::new(virgl);
        }
    }
    Box::new(StubRenderer::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::CAPSET_VENUS;

    #[test]
    fn submit_on_unknown_context_fails() {
        let mut gpu = HostGpu::for_tests();
        assert!(gpu.submit(1, &[], 0).is_err());
    }

    #[test]
    fn create_map_submit_fence() {
        let mut gpu = HostGpu::for_tests();
        gpu.ctx_create(1, CAPSET_VENUS, b"vk").expect("ctx");
        gpu.create_blob(1, 3, crate::state::BLOB_MEM_HOST3D, 4096)
            .expect("blob");
        let ipa = gpu.map_blob(3, 0).expect("map");
        assert!(ipa >= crate::HOSTMEM_GPA);
        let mut cs = Vec::new();
        cs.extend_from_slice(&1u32.to_le_bytes());
        cs.extend_from_slice(&8u32.to_le_bytes());
        gpu.submit(1, &cs, 42).expect("submit");
        gpu.mark_lost();
        assert!(gpu.is_lost());
        assert!(matches!(gpu.submit(1, &cs, 0), Err(Gpu3dError::GpuLost)));
    }
}
