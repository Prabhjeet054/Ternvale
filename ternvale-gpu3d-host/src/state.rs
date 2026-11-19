//! Contexts, blob resources, and fences. All ids come from the guest.

use std::collections::{HashMap, HashSet};

use crate::Gpu3dError;

/// `VIRTIO_GPU_BLOB_MEM_GUEST`.
pub const BLOB_MEM_GUEST: u32 = 0x0001;
/// `VIRTIO_GPU_BLOB_MEM_HOST3D`.
pub const BLOB_MEM_HOST3D: u32 = 0x0002;
/// `VIRTIO_GPU_BLOB_MEM_HOST3D_GUEST`.
pub const BLOB_MEM_HOST3D_GUEST: u32 = 0x0003;
/// Capset id in `context_init` low byte (`VIRTIO_GPU_F_CONTEXT_INIT`).
pub const CONTEXT_INIT_CAPSET_MASK: u32 = 0x0000_00ff;
/// `VIRTIO_GPU_CAPSET_VENUS`.
pub const CAPSET_VENUS: u32 = 4;
/// Maximum tracked fences (cross-device wait table).
pub const MAX_FENCES: usize = 4096;
/// Maximum 3D contexts.
pub const MAX_CONTEXTS: usize = 256;
/// Maximum blob resources.
pub const MAX_BLOBS: usize = 4096;

/// One blob resource.
#[derive(Debug, Clone)]
pub struct Blob {
    /// Guest blob size.
    pub size: u64,
    /// `VIRTIO_GPU_BLOB_MEM_*`.
    pub mem: u32,
}

/// Fence table: virtio `FLAG_FENCE` plus renderer timeline waits.
#[derive(Debug, Default)]
pub struct Fences {
    signaled: HashSet<u64>,
    pending: HashSet<u64>,
}

impl Fences {
    /// Record a fence the guest asked to signal after a command.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self),
        fields(fence_id)
    )]
    pub fn insert_pending(&mut self, fence_id: u64) -> Result<(), Gpu3dError> {
        if fence_id == 0 {
            return Err(Gpu3dError::BadId {
                id: 0,
                kind: "fence",
            });
        }
        if self.pending.len() + self.signaled.len() >= MAX_FENCES {
            return Err(Gpu3dError::Limit("too many fences"));
        }
        self.pending.insert(fence_id);
        Ok(())
    }

    /// Mark `fence_id` complete (host renderer or stub).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self),
        fields(fence_id)
    )]
    pub fn signal(&mut self, fence_id: u64) {
        self.pending.remove(&fence_id);
        self.signaled.insert(fence_id);
        tracing::trace!(target: "ternvale::gpu3d", fence_id, "gpu3d fence signaled");
    }

    /// Cross-device wait: true if already signaled.
    #[tracing::instrument(
        level = "trace",
        target = "ternvale::gpu3d",
        skip(self),
        fields(fence_id)
    )]
    pub fn is_signaled(&self, fence_id: u64) -> bool {
        self.signaled.contains(&fence_id)
    }
}

/// Live 3D objects for one virtio-gpu device.
#[derive(Debug, Default)]
pub struct Gpu3dState {
    contexts: HashSet<u32>,
    blobs: HashMap<u32, Blob>,
    /// Resource ids attached to a context.
    attached: HashMap<u32, HashSet<u32>>,
    /// Fence table.
    pub fences: Fences,
    lost: bool,
}

impl Gpu3dState {
    /// Empty device state.
    pub fn new() -> Self {
        Self::default()
    }

    /// True after [`Self::mark_lost`].
    pub fn is_lost(&self) -> bool {
        self.lost
    }

    /// Guest-fatal GPU loss: further submits fail; the VMM stays up.
    #[tracing::instrument(level = "error", target = "ternvale::gpu3d", skip(self))]
    pub fn mark_lost(&mut self) {
        self.lost = true;
        tracing::error!(
            target: "ternvale::gpu3d",
            "host GPU marked lost; VM continues without 3D"
        );
    }

    /// Create a Venus context.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self, debug_name),
        fields(ctx_id, context_init)
    )]
    pub fn ctx_create(
        &mut self,
        ctx_id: u32,
        context_init: u32,
        debug_name: &[u8],
    ) -> Result<(), Gpu3dError> {
        self.require_live()?;
        if ctx_id == 0 {
            return Err(Gpu3dError::BadId {
                id: 0,
                kind: "context",
            });
        }
        let capset = context_init & CONTEXT_INIT_CAPSET_MASK;
        if capset != CAPSET_VENUS {
            tracing::warn!(
                target: "ternvale::gpu3d",
                ctx_id,
                capset,
                "context_init capset is not Venus"
            );
            return Err(Gpu3dError::BadId {
                id: u64::from(capset),
                kind: "capset",
            });
        }
        if self.contexts.len() >= MAX_CONTEXTS && !self.contexts.contains(&ctx_id) {
            return Err(Gpu3dError::Limit("too many contexts"));
        }
        let name = String::from_utf8_lossy(debug_name);
        self.contexts.insert(ctx_id);
        self.attached.entry(ctx_id).or_default();
        tracing::info!(
            target: "ternvale::gpu3d",
            ctx_id,
            capset,
            name = %name,
            "3d context created"
        );
        Ok(())
    }

    /// Destroy a context if it exists.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self),
        fields(ctx_id)
    )]
    pub fn ctx_destroy(&mut self, ctx_id: u32) {
        if self.contexts.remove(&ctx_id) {
            self.attached.remove(&ctx_id);
            tracing::info!(target: "ternvale::gpu3d", ctx_id, "3d context destroyed");
        } else {
            tracing::warn!(target: "ternvale::gpu3d", ctx_id, "destroy unknown 3d context");
        }
    }

    /// Require `ctx_id` to exist.
    #[tracing::instrument(
        level = "trace",
        target = "ternvale::gpu3d",
        skip(self),
        fields(ctx_id)
    )]
    pub fn require_ctx(&self, ctx_id: u32) -> Result<(), Gpu3dError> {
        if ctx_id == 0 || !self.contexts.contains(&ctx_id) {
            return Err(Gpu3dError::BadId {
                id: u64::from(ctx_id),
                kind: "context",
            });
        }
        Ok(())
    }

    /// Create a blob resource.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self),
        fields(resource_id, blob_mem, size)
    )]
    pub fn create_blob(
        &mut self,
        resource_id: u32,
        blob_mem: u32,
        size: u64,
    ) -> Result<(), Gpu3dError> {
        self.require_live()?;
        if resource_id == 0 {
            return Err(Gpu3dError::BadId {
                id: 0,
                kind: "resource",
            });
        }
        if !matches!(
            blob_mem,
            BLOB_MEM_GUEST | BLOB_MEM_HOST3D | BLOB_MEM_HOST3D_GUEST
        ) {
            return Err(Gpu3dError::Mapping("blob_mem is invalid"));
        }
        if size == 0 {
            return Err(Gpu3dError::Mapping("blob size 0"));
        }
        if self.blobs.len() >= MAX_BLOBS && !self.blobs.contains_key(&resource_id) {
            return Err(Gpu3dError::Limit("too many blobs"));
        }
        self.blobs.insert(
            resource_id,
            Blob {
                size,
                mem: blob_mem,
            },
        );
        tracing::debug!(
            target: "ternvale::gpu3d",
            resource_id,
            blob_mem,
            size,
            "blob resource created"
        );
        Ok(())
    }

    /// Attach or detach a blob on a context.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self),
        fields(ctx_id, resource_id, attach)
    )]
    pub fn ctx_resource(
        &mut self,
        ctx_id: u32,
        resource_id: u32,
        attach: bool,
    ) -> Result<(), Gpu3dError> {
        self.require_ctx(ctx_id)?;
        if resource_id == 0 || !self.blobs.contains_key(&resource_id) {
            return Err(Gpu3dError::BadId {
                id: u64::from(resource_id),
                kind: "resource",
            });
        }
        let set = self.attached.entry(ctx_id).or_default();
        if attach {
            set.insert(resource_id);
        } else {
            set.remove(&resource_id);
        }
        Ok(())
    }

    /// Blob metadata if present.
    pub fn blob(&self, resource_id: u32) -> Result<&Blob, Gpu3dError> {
        self.blobs.get(&resource_id).ok_or(Gpu3dError::BadId {
            id: u64::from(resource_id),
            kind: "resource",
        })
    }

    fn require_live(&self) -> Result<(), Gpu3dError> {
        if self.lost {
            Err(Gpu3dError::GpuLost)
        } else {
            Ok(())
        }
    }

    /// Drop contexts and blobs (device reset).
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip(self))]
    pub fn reset(&mut self) {
        let contexts = self.contexts.len();
        let blobs = self.blobs.len();
        *self = Self::default();
        tracing::debug!(target: "ternvale::gpu3d", contexts, blobs, "gpu3d state reset");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn venus_context_and_blob() {
        let mut s = Gpu3dState::new();
        s.ctx_create(1, CAPSET_VENUS, b"vkcube").expect("ctx");
        s.create_blob(2, BLOB_MEM_HOST3D, 4096).expect("blob");
        s.ctx_resource(1, 2, true).expect("attach");
        s.fences.insert_pending(9).expect("fence");
        s.fences.signal(9);
        assert!(s.fences.is_signaled(9));
    }

    #[test]
    fn lost_gpu_rejects_new_work() {
        let mut s = Gpu3dState::new();
        s.mark_lost();
        assert!(matches!(
            s.ctx_create(1, CAPSET_VENUS, b"x"),
            Err(Gpu3dError::GpuLost)
        ));
    }
}
