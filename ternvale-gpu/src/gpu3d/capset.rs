//! Venus capset metadata (Linux `VIRTIO_GPU_CAPSET_VENUS` = 4).

use crate::protocol::{self, CtrlHdr, HDR_LEN};

/// `VIRTIO_GPU_CAPSET_VENUS` (Linux `virtio_gpu.h`).
pub const CAPSET_VENUS: u32 = 4;

/// One capset advertised when `gpu3d` is enabled.
pub const CAPSET_COUNT: u32 = 1;

/// Capset version Mesa's Venus driver negotiates today.
pub const VENUS_CAPSET_VERSION: u32 = 1;

/// `sizeof(struct virtio_gpu_resp_capset_info)` body after the header.
pub const CAPSET_INFO_BODY: usize = 12;

/// `sizeof(struct virtio_gpu_resp_capset_info)`.
pub const CAPSET_INFO_LEN: usize = HDR_LEN + CAPSET_INFO_BODY;

/// Venus renderer info blob size (virglrenderer `vkr_renderer_info` family).
/// TODO(verify): pin against the virglrenderer revision used with rutabaga_gfx.
pub const VENUS_CAPSET_BYTES: usize = 164;

/// Build `VIRTIO_GPU_RESP_OK_CAPSET_INFO` for capset index `index`.
#[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all, fields(index))]
pub fn capset_info_response(request: &CtrlHdr, index: u32) -> Result<Vec<u8>, CapsetError> {
    if index != 0 {
        tracing::warn!(target: "ternvale::gpu", index, "unknown virtio-gpu capset index");
        return Err(CapsetError::UnknownIndex { index });
    }
    let mut out = vec![0u8; CAPSET_INFO_LEN];
    out[..HDR_LEN].copy_from_slice(&protocol::response_hdr(
        request,
        protocol::RESP_OK_CAPSET_INFO,
    ));
    out[HDR_LEN..HDR_LEN + 4].copy_from_slice(&CAPSET_VENUS.to_le_bytes());
    out[HDR_LEN + 4..HDR_LEN + 8].copy_from_slice(&VENUS_CAPSET_VERSION.to_le_bytes());
    out[HDR_LEN + 8..HDR_LEN + 12].copy_from_slice(&(VENUS_CAPSET_BYTES as u32).to_le_bytes());
    tracing::debug!(
        target: "ternvale::gpu",
        capset_id = CAPSET_VENUS,
        version = VENUS_CAPSET_VERSION,
        size = VENUS_CAPSET_BYTES,
        "virtio-gpu capset info"
    );
    Ok(out)
}

/// Build `VIRTIO_GPU_RESP_OK_CAPSET` for Venus.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::gpu",
    skip_all,
    fields(capset_id, capset_version)
)]
pub fn capset_response(
    request: &CtrlHdr,
    capset_id: u32,
    capset_version: u32,
) -> Result<Vec<u8>, CapsetError> {
    if capset_id != CAPSET_VENUS {
        tracing::warn!(target: "ternvale::gpu", capset_id, "virtio-gpu capset not offered");
        return Err(CapsetError::UnknownCapset { capset_id });
    }
    if capset_version != VENUS_CAPSET_VERSION {
        tracing::warn!(
            target: "ternvale::gpu",
            capset_id,
            capset_version,
            want = VENUS_CAPSET_VERSION,
            "virtio-gpu capset version mismatch"
        );
        return Err(CapsetError::BadVersion {
            capset_id,
            capset_version,
        });
    }
    let mut out = vec![0u8; HDR_LEN + VENUS_CAPSET_BYTES];
    out[..HDR_LEN].copy_from_slice(&protocol::response_hdr(request, protocol::RESP_OK_CAPSET));
    super::virgl::fill_venus_capset(&mut out[HDR_LEN..]);
    tracing::trace!(
        target: "ternvale::gpu",
        capset_id,
        capset_version,
        bytes = VENUS_CAPSET_BYTES,
        "virtio-gpu venus capset returned"
    );
    Ok(out)
}

/// Minimal Venus capset payload: Vulkan 1.3 in the first `u32`, remainder zero until
/// virglrenderer/rutabaga fills real device limits.
/// Stub capset when virglrenderer is not loaded.
pub(crate) fn fill_venus_capset_stub(bytes: &mut [u8]) {
    bytes[0..4].copy_from_slice(&0x0040_0300u32.to_le_bytes()); // VK_API_VERSION_1_3
}

/// Capset negotiation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapsetError {
    /// `capset_index` is out of range.
    #[error("capset index {index} is not offered")]
    UnknownIndex {
        /// Guest index.
        index: u32,
    },
    /// `capset_id` is not Venus.
    #[error("capset id {capset_id} is not offered")]
    UnknownCapset {
        /// Guest id.
        capset_id: u32,
    },
    /// Version is not supported.
    #[error("capset {capset_id} version {capset_version} is not supported")]
    BadVersion {
        /// Guest id.
        capset_id: u32,
        /// Guest version.
        capset_version: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn venus_capset_info_lists_one_capset() {
        let hdr = CtrlHdr::default();
        let out = capset_info_response(&hdr, 0).expect("info");
        assert_eq!(out.len(), CAPSET_INFO_LEN);
        assert_eq!(
            u32::from_le_bytes(out[HDR_LEN..HDR_LEN + 4].try_into().unwrap()),
            CAPSET_VENUS
        );
    }
}
