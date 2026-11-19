//! Alignment for Hypervisor.framework maps and Metal / MoltenVK host-visible buffers.
//!
//! `hv_vm.h` requires page-aligned host address, IPA, and size. On Apple silicon
//! that page is **16 KiB** (`HOST_PAGE_SIZE` in `ternvale-vmm`).
//!
//! MoltenVK / Metal: `MTLBuffer` created with `newBufferWithBytesNoCopy` must be
//! page-aligned. Apple GPU linear texture alignment is typically 16 KiB or a
//! smaller power of two reported by `minimumLinearTextureAlignmentForPixelFormat`.
//! Venus `VK_EXT_external_memory_host` on MoltenVK is incomplete upstream;
//! see `docs/GPU_ACCELERATION.md`.
//!
//! TODO(verify): pin `MOLTENVK_HOST_ALIGN` against the MoltenVK revision used
//! with virglrenderer Venus on macOS 15–26.

/// Hypervisor.framework / Apple silicon host page (16 KiB).
pub const HV_PAGE: u64 = 0x4000;
/// Conservative host-visible blob alignment (same as the HV page).
pub const MOLTENVK_HOST_ALIGN: u64 = HV_PAGE;
/// virtio-gpu `blob_alignment` advertised to the guest (4 KiB, virtio 1.2).
pub const VIRTIO_BLOB_ALIGN: u64 = 0x1000;
/// Guest-physical BAR window for host-visible blobs (above RAM and PCIe).
/// 64 GiB, 16 KiB aligned. Must not overlap `RAM_BASE` (1 GiB) + guest RAM.
pub const HOSTMEM_GPA: u64 = 0x10_0000_0000;
/// Maximum host-visible window mapped at once (256 MiB).
pub const HOSTMEM_BYTES: u64 = 256 * 1024 * 1024;

/// True when `value` is a non-zero multiple of `align`.
#[tracing::instrument(level = "trace", target = "ternvale::gpu3d", skip_all)]
pub fn is_aligned(value: u64, align: u64) -> bool {
    align != 0 && value % align == 0
}

/// Round `value` up to `align` (power of two). None on overflow or `align == 0`.
#[tracing::instrument(level = "trace", target = "ternvale::gpu3d", skip_all)]
pub fn align_up(value: u64, align: u64) -> Option<u64> {
    if align == 0 || !align.is_power_of_two() {
        return None;
    }
    let mask = align - 1;
    value.checked_add(mask).map(|sum| sum & !mask)
}

/// Host mapping size: at least `size`, aligned to MoltenVK/HV.
#[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all, fields(size))]
pub fn map_bytes(size: u64) -> Result<u64, crate::Gpu3dError> {
    if size == 0 {
        return Err(crate::Gpu3dError::Mapping("blob size 0"));
    }
    align_up(size, MOLTENVK_HOST_ALIGN).ok_or(crate::Gpu3dError::Mapping("size overflow"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostmem_window_is_page_aligned() {
        assert!(is_aligned(HOSTMEM_GPA, HV_PAGE));
        assert!(is_aligned(HOSTMEM_BYTES, HV_PAGE));
        assert_eq!(map_bytes(1).expect("map"), MOLTENVK_HOST_ALIGN);
        assert!(map_bytes(0).is_err());
    }
}
