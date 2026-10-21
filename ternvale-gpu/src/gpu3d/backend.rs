//! In-process Venus backend (rutabaga_gfx-shaped command handling).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use ternvale_vmm::GuestMemory;

use crate::gpu2d::GpuStats;
use crate::gpu3d::capset::{self, CapsetError, CAPSET_VENUS};
use crate::protocol::{self, Command, CtrlHdr, MemEntry};

/// Guest blob memory kinds (`virtio_gpu.h`).
const BLOB_MEM_GUEST: u32 = 0x0001;
const BLOB_MEM_HOST3D: u32 = 0x0002;
const BLOB_MEM_HOST3D_GUEST: u32 = 0x0003;

const CONTEXT_INIT_CAPSET_MASK: u32 = 0x0000_00ff;

type Fail = (u32, String);

/// Tracked 3D state for one virtio-gpu device.
pub struct Gpu3d {
    stats: Arc<GpuStats>,
    contexts: HashSet<u32>,
    blobs: HashMap<u32, BlobResource>,
    virgl: bool,
}

struct BlobResource {
    size: u64,
    entries: Vec<MemEntry>,
}

impl Gpu3d {
    /// Empty 3D state.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn new(stats: Arc<GpuStats>) -> Self {
        let virgl = super::virgl::ensure_loaded();
        tracing::info!(
            target: "ternvale::gpu",
            virgl,
            backend = if virgl { "virgl-venus" } else { "venus-stub" },
            "virtio-gpu 3d backend ready"
        );
        Self {
            stats,
            contexts: HashSet::new(),
            blobs: HashMap::new(),
            virgl,
        }
    }

    /// Drop contexts and blob metadata (2D resources are handled separately).
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn reset(&mut self) {
        let contexts = self.contexts.len();
        let blobs = self.blobs.len();
        self.contexts.clear();
        self.blobs.clear();
        tracing::debug!(target: "ternvale::gpu", contexts, blobs, "virtio-gpu 3d state reset");
    }

    /// Handle one 3D-related command. Returns an empty vec when the answer is only a header
    /// written by the caller (capset responses include their own header).
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn handle(
        &mut self,
        mem: &GuestMemory,
        hdr: &CtrlHdr,
        command: &Command,
    ) -> Result<Vec<u8>, Fail> {
        match command {
            Command::GetCapsetInfo { capset_index } => {
                match capset::capset_info_response(hdr, *capset_index) {
                    Ok(bytes) => Ok(bytes),
                    Err(CapsetError::UnknownIndex { index }) => Err((
                        protocol::RESP_ERR_INVALID_PARAMETER,
                        format!("capset index {index} is invalid"),
                    )),
                    Err(error) => Err((protocol::RESP_ERR_UNSPEC, error.to_string())),
                }
            }
            Command::GetCapset {
                capset_id,
                capset_version,
            } => match capset::capset_response(hdr, *capset_id, *capset_version) {
                Ok(bytes) => Ok(bytes),
                Err(CapsetError::UnknownCapset { capset_id }) => Err((
                    protocol::RESP_ERR_INVALID_PARAMETER,
                    format!("capset {capset_id} is not offered"),
                )),
                Err(CapsetError::BadVersion {
                    capset_id,
                    capset_version,
                }) => Err((
                    protocol::RESP_ERR_INVALID_PARAMETER,
                    format!("capset {capset_id} version {capset_version} is not supported"),
                )),
                Err(CapsetError::UnknownIndex { .. }) => Err((
                    protocol::RESP_ERR_UNSPEC,
                    "internal capset error".to_string(),
                )),
            },
            Command::ResourceCreateBlob {
                resource_id,
                blob_mem,
                blob_flags,
                entries,
                blob_id,
                size,
            } => self.create_blob(
                *resource_id,
                *blob_mem,
                *blob_flags,
                *blob_id,
                *size,
                entries.clone(),
            ),
            Command::CtxCreate {
                nlen,
                context_init,
                debug_name,
            } => self.ctx_create(hdr.ctx_id, *nlen, *context_init, debug_name),
            Command::CtxDestroy => self.ctx_destroy(hdr.ctx_id),
            Command::CtxResource { resource_id } => {
                self.ctx_touch_resource(hdr.ctx_id, *resource_id)
            }
            Command::Submit3d { size, payload } => self.submit_3d(hdr, *size, payload),
            Command::BlobMap {
                resource_id,
                offset,
            } => self.map_blob(mem, *resource_id, *offset),
            _ => Err((protocol::RESP_ERR_UNSPEC, "not a 3d command".to_string())),
        }
    }

    fn create_blob(
        &mut self,
        resource_id: u32,
        blob_mem: u32,
        blob_flags: u32,
        blob_id: u64,
        size: u64,
        entries: Vec<MemEntry>,
    ) -> Result<Vec<u8>, Fail> {
        if resource_id == 0 {
            return Err((
                protocol::RESP_ERR_INVALID_RESOURCE_ID,
                "blob resource id 0 is invalid".to_string(),
            ));
        }
        if !matches!(
            blob_mem,
            BLOB_MEM_GUEST | BLOB_MEM_HOST3D | BLOB_MEM_HOST3D_GUEST
        ) {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                format!("blob_mem {blob_mem:#x} is invalid"),
            ));
        }
        if size == 0 {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                "blob size 0 is invalid".to_string(),
            ));
        }
        for entry in &entries {
            if entry.len == 0 || entry.addr == 0 {
                return Err((
                    protocol::RESP_ERR_INVALID_PARAMETER,
                    "blob mem entry has zero addr or len".to_string(),
                ));
            }
        }
        tracing::debug!(
            target: "ternvale::gpu",
            resource_id,
            blob_mem,
            blob_flags,
            blob_id,
            size,
            entries = entries.len(),
            "virtio-gpu resource create blob"
        );
        tracing::trace!(
            target: "ternvale::gpu",
            blob_mem,
            blob_flags,
            blob_id,
            "virtio-gpu blob metadata recorded"
        );
        self.blobs
            .insert(resource_id, BlobResource { size, entries });
        Ok(Vec::new())
    }

    fn ctx_create(
        &mut self,
        ctx_id: u32,
        nlen: u32,
        context_init: u32,
        debug_name: &[u8; 64],
    ) -> Result<Vec<u8>, Fail> {
        if ctx_id == 0 {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                "context id 0 is invalid".to_string(),
            ));
        }
        let capset = context_init & CONTEXT_INIT_CAPSET_MASK;
        if capset != CAPSET_VENUS {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                format!("context capset {capset} is not Venus ({CAPSET_VENUS})"),
            ));
        }
        let name_end = nlen.min(64) as usize;
        let name = String::from_utf8_lossy(&debug_name[..name_end]);
        if !self.contexts.insert(ctx_id) {
            tracing::warn!(target: "ternvale::gpu", ctx_id, "virtio-gpu context id reused");
        }
        if self.virgl && !super::virgl::context_create(ctx_id, name.as_ref()) {
            return Err((
                protocol::RESP_ERR_UNSPEC,
                "virgl_renderer_context_create failed".to_string(),
            ));
        }
        self.stats.contexts.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            target: "ternvale::gpu",
            ctx_id,
            capset,
            name = %name,
            virgl = self.virgl,
            "virtio-gpu 3d context created"
        );
        Ok(Vec::new())
    }

    fn ctx_destroy(&mut self, ctx_id: u32) -> Result<Vec<u8>, Fail> {
        if !self.contexts.remove(&ctx_id) {
            tracing::warn!(target: "ternvale::gpu", ctx_id, "destroy unknown 3d context");
        } else {
            if self.virgl {
                super::virgl::context_destroy(ctx_id);
            }
            tracing::info!(target: "ternvale::gpu", ctx_id, "virtio-gpu 3d context destroyed");
        }
        Ok(Vec::new())
    }

    fn ctx_touch_resource(&mut self, ctx_id: u32, resource_id: u32) -> Result<Vec<u8>, Fail> {
        if !self.contexts.contains(&ctx_id) {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                format!("unknown context {ctx_id}"),
            ));
        }
        if resource_id == 0 || !self.blobs.contains_key(&resource_id) {
            return Err((
                protocol::RESP_ERR_INVALID_RESOURCE_ID,
                format!("unknown blob resource {resource_id}"),
            ));
        }
        tracing::trace!(
            target: "ternvale::gpu",
            ctx_id,
            resource_id,
            "virtio-gpu context blob attach/detach"
        );
        Ok(Vec::new())
    }

    fn submit_3d(&mut self, hdr: &CtrlHdr, size: u32, payload: &[u8]) -> Result<Vec<u8>, Fail> {
        if hdr.ctx_id == 0 || !self.contexts.contains(&hdr.ctx_id) {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                format!("submit_3d on unknown context {}", hdr.ctx_id),
            ));
        }
        if payload.len() != size as usize {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                "submit_3d size does not match payload".to_string(),
            ));
        }
        self.stats.submits.fetch_add(1, Ordering::Relaxed);
        tracing::trace!(
            target: "ternvale::gpu",
            ctx_id = hdr.ctx_id,
            bytes = payload.len(),
            virgl = self.virgl,
            "virtio-gpu submit_3d"
        );
        if self.virgl {
            match super::virgl::submit_3d(hdr.ctx_id, payload) {
                Ok(()) => return Ok(Vec::new()),
                Err(rc) => {
                    return Err((
                        protocol::RESP_ERR_UNSPEC,
                        format!("virgl_renderer_submit_cmd failed with {rc}"),
                    ));
                }
            }
        }
        Err((
            protocol::RESP_ERR_UNSPEC,
            "venus rendering is not available (install libvirglrenderer or set TERNVALE_VIRGL_RENDERER)".to_string(),
        ))
    }

    fn map_blob(
        &mut self,
        mem: &GuestMemory,
        resource_id: u32,
        offset: u64,
    ) -> Result<Vec<u8>, Fail> {
        let blob = self.blobs.get(&resource_id).ok_or((
            protocol::RESP_ERR_INVALID_RESOURCE_ID,
            format!("unknown blob resource {resource_id}"),
        ))?;
        if offset >= blob.size {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                format!("map offset {offset} past blob size {}", blob.size),
            ));
        }
        for entry in &blob.entries {
            if entry.len == 0 {
                continue;
            }
            let mut scratch = [0u8; 8];
            if mem.read_bytes(entry.addr, &mut scratch).is_err() {
                tracing::warn!(
                    target: "ternvale::gpu",
                    resource_id,
                    addr = format!("{:#x}", entry.addr),
                    "blob guest page unreadable during map"
                );
            }
        }
        tracing::trace!(
            target: "ternvale::gpu",
            resource_id,
            offset,
            size = blob.size,
            "virtio-gpu resource map blob"
        );
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{CMD_GET_CAPSET, CMD_GET_CAPSET_INFO, HDR_LEN};

    #[test]
    fn capset_round_trip() {
        let stats = Arc::new(GpuStats::default());
        let mut gpu = Gpu3d::new(stats);
        let mem = crate::testutil::memory();
        let mem = mem.lock().expect("mem");
        let mut req = vec![0u8; HDR_LEN + 4];
        req[0..4].copy_from_slice(&CMD_GET_CAPSET_INFO.to_le_bytes());
        req[HDR_LEN..HDR_LEN + 4].copy_from_slice(&0u32.to_le_bytes());
        let (hdr, cmd) = protocol::parse(&req).expect("parse");
        let out = gpu.handle(&mem, &hdr, &cmd).expect("info");
        assert!(out.len() > HDR_LEN);

        let mut req2 = vec![0u8; HDR_LEN + 8];
        req2[0..4].copy_from_slice(&CMD_GET_CAPSET.to_le_bytes());
        req2[HDR_LEN..HDR_LEN + 4].copy_from_slice(&CAPSET_VENUS.to_le_bytes());
        req2[HDR_LEN + 4..HDR_LEN + 8].copy_from_slice(&1u32.to_le_bytes());
        let (hdr2, cmd2) = protocol::parse(&req2).expect("parse2");
        let cap = gpu.handle(&mem, &hdr2, &cmd2).expect("capset");
        assert_eq!(cap.len(), HDR_LEN + capset::VENUS_CAPSET_BYTES);
    }
}
