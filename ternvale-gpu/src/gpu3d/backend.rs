//! virtio-gpu 3D commands delegated to `ternvale-gpu3d-host`.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use ternvale_gpu3d_host::{HostGpu, BLOB_MEM_GUEST};
use ternvale_vmm::GuestMemory;

use crate::gpu2d::GpuStats;
use crate::gpu3d::capset::{self, CapsetError};
use crate::protocol::{self, Command, CtrlHdr};

type Fail = (u32, String);

/// Tracked 3D state for one virtio-gpu device.
pub struct Gpu3d {
    stats: Arc<GpuStats>,
    host: HostGpu,
}

impl Gpu3d {
    /// Empty 3D state with the host renderer (virglrenderer if loaded).
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn new(stats: Arc<GpuStats>) -> Self {
        tracing::info!(
            target: "ternvale::gpu",
            "virtio-gpu 3d path using ternvale-gpu3d-host"
        );
        Self {
            stats,
            host: HostGpu::new().with_hv_mapper(),
        }
    }

    /// Tests: stub renderer that accepts SUBMIT_3D.
    #[cfg(test)]
    pub fn for_tests(stats: Arc<GpuStats>) -> Self {
        Self {
            stats,
            host: HostGpu::for_tests(),
        }
    }

    /// Drop contexts and blob metadata.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn reset(&mut self) {
        self.host.reset();
        tracing::debug!(target: "ternvale::gpu", "virtio-gpu 3d state reset");
    }

    /// Handle one 3D-related command.
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
            } => {
                if *blob_mem == BLOB_MEM_GUEST {
                    for entry in entries {
                        if entry.len == 0 || entry.addr == 0 {
                            return Err((
                                protocol::RESP_ERR_INVALID_PARAMETER,
                                "blob mem entry has zero addr or len".to_string(),
                            ));
                        }
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
                self.host
                    .create_blob(hdr.ctx_id, *resource_id, *blob_mem, *size)
                    .map_err(fail)
                    .map(|()| Vec::new())
            }
            Command::CtxCreate {
                nlen,
                context_init,
                debug_name,
            } => self.ctx_create(hdr.ctx_id, *nlen, *context_init, debug_name),
            Command::CtxDestroy => {
                self.host.ctx_destroy(hdr.ctx_id);
                Ok(Vec::new())
            }
            Command::CtxResource { resource_id } => self
                .host
                .ctx_resource(hdr.ctx_id, *resource_id, true)
                .map_err(fail)
                .map(|()| Vec::new()),
            Command::Submit3d { size, payload } => self.submit_3d(hdr, *size, payload),
            Command::BlobMap {
                resource_id,
                offset,
            } => self.map_blob(mem, hdr.kind, *resource_id, *offset),
            _ => Err((protocol::RESP_ERR_UNSPEC, "not a 3d command".to_string())),
        }
    }

    fn ctx_create(
        &mut self,
        ctx_id: u32,
        nlen: u32,
        context_init: u32,
        debug_name: &[u8; 64],
    ) -> Result<Vec<u8>, Fail> {
        let name_end = nlen.min(64) as usize;
        self.host
            .ctx_create(ctx_id, context_init, &debug_name[..name_end])
            .map_err(fail)?;
        self.stats.contexts.fetch_add(1, Ordering::Relaxed);
        Ok(Vec::new())
    }

    fn submit_3d(&mut self, hdr: &CtrlHdr, size: u32, payload: &[u8]) -> Result<Vec<u8>, Fail> {
        if payload.len() != size as usize {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                "submit_3d size does not match payload".to_string(),
            ));
        }
        let fence = if hdr.flags & protocol::FLAG_FENCE != 0 {
            hdr.fence_id
        } else {
            0
        };
        self.stats.submits.fetch_add(1, Ordering::Relaxed);
        match self.host.submit(hdr.ctx_id, payload, fence) {
            Ok(()) => Ok(Vec::new()),
            Err(error) if matches!(error, ternvale_gpu3d_host::Gpu3dError::GpuLost) => {
                tracing::error!(
                    target: "ternvale::gpu3d",
                    ctx_id = hdr.ctx_id,
                    "host GPU lost during submit_3d; VM continues"
                );
                Err(fail(error))
            }
            Err(error) => Err(fail(error)),
        }
    }

    fn map_blob(
        &mut self,
        mem: &GuestMemory,
        kind: u32,
        resource_id: u32,
        offset: u64,
    ) -> Result<Vec<u8>, Fail> {
        if kind == protocol::CMD_RESOURCE_UNMAP_BLOB {
            return self
                .host
                .unmap_blob(resource_id)
                .map_err(fail)
                .map(|()| Vec::new());
        }
        let _ = mem;
        let ipa = self.host.map_blob(resource_id, offset).map_err(fail)?;
        tracing::trace!(
            target: "ternvale::gpu",
            resource_id,
            offset,
            ipa = format!("{:#x}", ipa),
            "virtio-gpu resource map blob"
        );
        Ok(Vec::new())
    }
}

fn fail(error: ternvale_gpu3d_host::Gpu3dError) -> Fail {
    (error.virtio_status(), error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{CMD_GET_CAPSET, CMD_GET_CAPSET_INFO, HDR_LEN};
    use ternvale_gpu3d_host::CAPSET_VENUS;

    #[test]
    fn capset_round_trip() {
        let stats = Arc::new(GpuStats::default());
        let mut gpu = Gpu3d::for_tests(stats);
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
