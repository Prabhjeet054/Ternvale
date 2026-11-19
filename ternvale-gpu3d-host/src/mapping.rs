//! Host-visible blob memory: allocate aligned host pages and map them at a
//! guest IPA BAR window with `hv_vm_map`.

use std::collections::HashMap;
use std::ptr::NonNull;

use crate::align::{
    align_up, is_aligned, map_bytes, HOSTMEM_BYTES, HOSTMEM_GPA, HV_PAGE, MOLTENVK_HOST_ALIGN,
};
use crate::Gpu3dError;

/// Map or unmap host pages into the current VM.
pub trait GuestMapper: Send {
    /// Map `size` bytes at `host` to guest IPA `ipa`.
    fn map(&mut self, host: NonNull<u8>, ipa: u64, size: usize) -> Result<(), Gpu3dError>;
    /// Unmap a previous mapping.
    fn unmap(&mut self, ipa: u64, size: usize) -> Result<(), Gpu3dError>;
}

/// `hv_vm_map` / `hv_vm_unmap` (process-wide VM).
pub struct HvMapper;

impl GuestMapper for HvMapper {
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self, host),
        fields(ipa = format!("{:#x}", ipa), size)
    )]
    fn map(&mut self, host: NonNull<u8>, ipa: u64, size: usize) -> Result<(), Gpu3dError> {
        let flags = ternvale_hv::HV_MEMORY_READ | ternvale_hv::HV_MEMORY_WRITE;
        ternvale_hv::map_memory(host, ipa, size, flags).map_err(|error| {
            tracing::error!(
                target: "ternvale::gpu3d",
                error = %error,
                ipa = format!("{:#x}", ipa),
                "hv_vm_map of gpu3d hostmem failed"
            );
            Gpu3dError::Mapping("hv_vm_map failed")
        })
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self),
        fields(ipa = format!("{:#x}", ipa), size)
    )]
    fn unmap(&mut self, ipa: u64, size: usize) -> Result<(), Gpu3dError> {
        ternvale_hv::unmap_memory(ipa, size).map_err(|error| {
            tracing::error!(
                target: "ternvale::gpu3d",
                error = %error,
                ipa = format!("{:#x}", ipa),
                "hv_vm_unmap of gpu3d hostmem failed"
            );
            Gpu3dError::Mapping("hv_vm_unmap failed")
        })
    }
}

/// Records map calls; used in tests without a VM.
#[derive(Default)]
pub struct RecordingMapper {
    /// `(ipa, size)` currently mapped.
    pub mapped: Vec<(u64, usize)>,
}

impl GuestMapper for RecordingMapper {
    fn map(&mut self, _host: NonNull<u8>, ipa: u64, size: usize) -> Result<(), Gpu3dError> {
        self.mapped.push((ipa, size));
        Ok(())
    }

    fn unmap(&mut self, ipa: u64, size: usize) -> Result<(), Gpu3dError> {
        self.mapped.retain(|entry| *entry != (ipa, size));
        Ok(())
    }
}

struct Slot {
    /// Host pages kept alive for the HV map (pointer passed to `hv_vm_map`).
    #[allow(dead_code)]
    host: Vec<u8>,
    ipa: u64,
    size: usize,
}

/// Bump allocator in the hostmem BAR window.
pub struct HostVisiblePool {
    next: u64,
    slots: HashMap<u32, Slot>,
}

impl HostVisiblePool {
    /// Empty pool at [`HOSTMEM_GPA`].
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all)]
    pub fn new() -> Self {
        tracing::info!(
            target: "ternvale::gpu3d",
            gpa = format!("{:#x}", HOSTMEM_GPA),
            bytes = HOSTMEM_BYTES,
            hv_page = HV_PAGE,
            molten_align = MOLTENVK_HOST_ALIGN,
            "gpu3d host-visible BAR window"
        );
        Self {
            next: HOSTMEM_GPA,
            slots: HashMap::new(),
        }
    }

    /// Allocate, align, and map `size` bytes for blob `resource_id`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self, mapper),
        fields(resource_id, size)
    )]
    pub fn map_blob(
        &mut self,
        mapper: &mut dyn GuestMapper,
        resource_id: u32,
        size: u64,
        offset: u64,
    ) -> Result<u64, Gpu3dError> {
        if resource_id == 0 {
            return Err(Gpu3dError::BadId {
                id: 0,
                kind: "resource",
            });
        }
        if self.slots.contains_key(&resource_id) {
            return Err(Gpu3dError::Mapping("blob already mapped"));
        }
        if offset != 0 && !is_aligned(offset, VIRTIO_ALIGN) {
            return Err(Gpu3dError::Mapping("map offset is not blob-aligned"));
        }
        let bytes = map_bytes(size)?;
        if bytes > HOSTMEM_BYTES {
            return Err(Gpu3dError::Limit("blob larger than hostmem window"));
        }
        let ipa = match align_up(self.next.saturating_add(offset), MOLTENVK_HOST_ALIGN) {
            Some(ipa) => ipa,
            None => return Err(Gpu3dError::Mapping("ipa overflow")),
        };
        if ipa < HOSTMEM_GPA
            || ipa.saturating_add(bytes) > HOSTMEM_GPA.saturating_add(HOSTMEM_BYTES)
        {
            tracing::warn!(
                target: "ternvale::gpu3d",
                resource_id,
                ipa = format!("{:#x}", ipa),
                bytes,
                "hostmem BAR exhausted"
            );
            return Err(Gpu3dError::Limit("hostmem BAR exhausted"));
        }
        let mut host = vec![0u8; bytes as usize];
        let ptr = NonNull::new(host.as_mut_ptr()).ok_or(Gpu3dError::Mapping("null host buffer"))?;
        mapper.map(ptr, ipa, bytes as usize)?;
        self.slots.insert(
            resource_id,
            Slot {
                host,
                ipa,
                size: bytes as usize,
            },
        );
        self.next = ipa.saturating_add(bytes);
        tracing::debug!(
            target: "ternvale::gpu3d",
            resource_id,
            ipa = format!("{:#x}", ipa),
            bytes,
            "mapped host-visible blob into BAR"
        );
        Ok(ipa)
    }

    /// Unmap a blob if present.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self, mapper),
        fields(resource_id)
    )]
    pub fn unmap_blob(
        &mut self,
        mapper: &mut dyn GuestMapper,
        resource_id: u32,
    ) -> Result<(), Gpu3dError> {
        let Some(slot) = self.slots.remove(&resource_id) else {
            return Err(Gpu3dError::BadId {
                id: u64::from(resource_id),
                kind: "resource",
            });
        };
        mapper.unmap(slot.ipa, slot.size)?;
        tracing::debug!(
            target: "ternvale::gpu3d",
            resource_id,
            ipa = format!("{:#x}", slot.ipa),
            "unmapped host-visible blob"
        );
        Ok(())
    }
}

impl Default for HostVisiblePool {
    fn default() -> Self {
        Self::new()
    }
}

const VIRTIO_ALIGN: u64 = crate::align::VIRTIO_BLOB_ALIGN;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_aligned_blob_in_bar() {
        let mut pool = HostVisiblePool::new();
        let mut mapper = RecordingMapper::default();
        let ipa = pool.map_blob(&mut mapper, 1, 1, 0).expect("map");
        assert!(is_aligned(ipa, MOLTENVK_HOST_ALIGN));
        assert_eq!(mapper.mapped.len(), 1);
        pool.unmap_blob(&mut mapper, 1).expect("unmap");
        assert!(mapper.mapped.is_empty());
    }

    #[test]
    fn rejects_id_zero() {
        let mut pool = HostVisiblePool::new();
        let mut mapper = RecordingMapper::default();
        assert!(pool.map_blob(&mut mapper, 0, 4096, 0).is_err());
    }
}
