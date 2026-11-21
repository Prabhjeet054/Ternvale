//! Dirty-page tracking and lazy file-backed remap for [`super::GuestMemory`].

use std::sync::atomic::Ordering;

use super::dirty::DirtyBits;
use super::host::{mmap_file_private, munmap_region};
use super::{GuestMemory, MemoryError, HOST_PAGE_SIZE};

impl GuestMemory {
    /// Host pointer for a **write** into guest RAM. Marks pages dirty.
    ///
    /// Device DMA (virtio-blk `preadv` into guest buffers) must use this, not
    /// [`Self::host_ptr`], which never faults at stage 2.
    #[tracing::instrument(
        level = "trace",
        target = "ternvale::mem",
        skip_all,
        fields(gpa = format!("{:#x}", gpa), size = len)
    )]
    pub fn host_ptr_mut(&self, gpa: u64, len: usize) -> Result<*mut u8, MemoryError> {
        let ptr = self.host_ptr(gpa, len)?;
        self.mark_dirty(gpa, len);
        Ok(ptr)
    }

    /// Record host-side writes for incremental snapshots.
    #[tracing::instrument(level = "debug", target = "ternvale::mem", skip_all)]
    pub fn set_dirty_tracking(&self, on: bool) {
        self.tracking.store(on, Ordering::Release);
        tracing::info!(target: "ternvale::mem", on, "dirty-page tracking");
    }

    /// Write-protect mapped RAM at stage 2 (`hv_vm_protect` → RX).
    #[tracing::instrument(level = "debug", target = "ternvale::mem", skip_all)]
    pub fn write_protect_ram(&self) -> Result<(), MemoryError> {
        self.set_dirty_tracking(true);
        let started = std::time::Instant::now();
        for region in &self.regions {
            if !region.guest_mapped || region.flags & ternvale_hv::HV_MEMORY_WRITE == 0 {
                continue;
            }
            if let Err(source) =
                ternvale_hv::protect_memory(region.gpa, region.size, ternvale_hv::HV_MEMORY_RX)
            {
                return Err(MemoryError::Protect {
                    gpa: region.gpa,
                    size: region.size as u64,
                    source,
                });
            }
        }
        tracing::debug!(
            target: "ternvale::mem",
            protect_ns = started.elapsed().as_nanos() as u64,
            "stage-2 write-protect completed"
        );
        Ok(())
    }

    /// Guest store to a write-protected page: mark dirty and restore RWX.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::mem",
        skip_all,
        fields(gpa = format!("{:#x}", gpa))
    )]
    pub fn note_stage2_write(&self, gpa: u64) -> Result<bool, MemoryError> {
        let Ok(region) = self.access(gpa, 1, "dirty-fault") else {
            return Ok(false);
        };
        self.mark_dirty(gpa, 1);
        if region.guest_mapped {
            let page = gpa - (gpa % HOST_PAGE_SIZE);
            if let Err(source) = ternvale_hv::protect_memory(
                page,
                HOST_PAGE_SIZE as usize,
                ternvale_hv::HV_MEMORY_RWX,
            ) {
                return Err(MemoryError::Protect {
                    gpa: page,
                    size: HOST_PAGE_SIZE,
                    source,
                });
            }
        }
        Ok(true)
    }

    /// Copy dirty pages and clear the bitmap (incremental snapshot).
    #[tracing::instrument(level = "debug", target = "ternvale::mem", skip_all)]
    pub fn take_dirty_pages(&self) -> Result<Vec<(u64, Vec<u8>)>, MemoryError> {
        let page = HOST_PAGE_SIZE as usize;
        let mut out = Vec::new();
        for region in &self.regions {
            for index in region.dirty.take() {
                let gpa = region.gpa + (index as u64) * HOST_PAGE_SIZE;
                let mut buf = vec![0u8; page];
                self.read_bytes(gpa, &mut buf)?;
                out.push((gpa, buf));
            }
        }
        tracing::info!(
            target: "ternvale::mem",
            pages = out.len(),
            "collected dirty pages"
        );
        Ok(out)
    }

    /// True when tracking is on and at least one page is dirty.
    #[tracing::instrument(level = "trace", target = "ternvale::mem", skip_all)]
    pub fn has_dirty_pages(&self) -> bool {
        self.tracking.load(Ordering::Acquire) && self.regions.iter().any(|r| !r.dirty.is_empty())
    }

    /// Replace a region's host pages with a private mapping of `path` (lazy restore).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::mem",
        skip_all,
        fields(gpa = format!("{:#x}", gpa), path = %path.display())
    )]
    pub fn remap_file_private(
        &mut self,
        gpa: u64,
        path: &std::path::Path,
    ) -> Result<(), MemoryError> {
        let index = self.find(gpa, HOST_PAGE_SIZE as usize)?;
        let (size, flags, mapped) = {
            let region = &self.regions[index];
            (region.size, region.flags, region.guest_mapped)
        };
        if mapped {
            ternvale_hv::unmap_memory(gpa, size).map_err(|source| MemoryError::Unmap {
                gpa,
                size: size as u64,
                source,
            })?;
        }
        let old_host = self.regions[index].host;
        munmap_region(old_host, size)?;
        let host = mmap_file_private(path, size)?;
        if mapped {
            if let Err(source) = ternvale_hv::map_memory(host, gpa, size, flags) {
                let _ = munmap_region(host, size);
                return Err(MemoryError::Map {
                    gpa,
                    size: size as u64,
                    source,
                });
            }
        }
        self.regions[index].host = host;
        self.regions[index].dirty = DirtyBits::new(size);
        tracing::info!(
            target: "ternvale::mem",
            gpa = format!("{gpa:#x}"),
            path = %path.display(),
            size,
            "lazy-restored RAM from file-backed mmap"
        );
        Ok(())
    }

    pub(super) fn mark_dirty(&self, gpa: u64, len: usize) {
        if !self.tracking.load(Ordering::Acquire) || len == 0 {
            return;
        }
        if let Ok(region) = self.find(gpa, len) {
            let offset = (gpa - self.regions[region].gpa) as usize;
            self.regions[region].dirty.mark_offset(offset, len);
        }
    }
}
