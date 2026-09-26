//! Guest physical memory: host `mmap` regions and `hv_vm_map`.
//!
//! Integer helpers use little-endian, matching an AArch64 guest. An access must
//! lie entirely in one region. Drop unmaps guest mappings before releasing the
//! host pages. Drop [`GuestMemory`] before the [`ternvale_hv::Vm`].

mod error;
mod host;

use std::ptr::NonNull;

pub use error::MemoryError;
pub use host::HOST_PAGE_SIZE;

use host::{host_page_size, mmap_anonymous, munmap_region};

#[rustfmt::skip]
macro_rules! guest_int {
    ($read:ident, $write:ident, $ty:ty) => {
        /// Read a little-endian value at `gpa`.
        #[tracing::instrument(
            level = "debug",
            target = "ternvale::mem",
            skip_all,
            fields(gpa = format!("{:#x}", gpa))
        )]
        pub fn $read(&self, gpa: u64) -> Result<$ty, MemoryError> {
            let mut buf = [0u8; std::mem::size_of::<$ty>()];
            self.read_bytes(gpa, &mut buf)?;
            Ok(<$ty>::from_le_bytes(buf))
        }

        /// Write a little-endian value at `gpa`.
        #[tracing::instrument(
            level = "debug",
            target = "ternvale::mem",
            skip_all,
            fields(gpa = format!("{:#x}", gpa))
        )]
        pub fn $write(&mut self, gpa: u64, value: $ty) -> Result<(), MemoryError> {
            self.write_bytes(gpa, &value.to_le_bytes())
        }
    };
}

/// One contiguous guest physical range backed by an anonymous host mapping.
struct Region {
    gpa: u64,
    size: usize,
    host: NonNull<u8>,
    guest_mapped: bool,
}

impl Region {
    fn end(&self) -> u64 {
        self.gpa + self.size as u64
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        if self.guest_mapped {
            match ternvale_hv::unmap_memory(self.gpa, self.size) {
                Ok(()) => tracing::info!(
                    target: "ternvale::mem",
                    gpa = format!("{:#x}", self.gpa),
                    size = format!("{:#x}", self.size),
                    "unmapped guest region"
                ),
                Err(error) => tracing::error!(
                    target: "ternvale::mem",
                    gpa = format!("{:#x}", self.gpa),
                    size = format!("{:#x}", self.size),
                    error = %error,
                    "hv_vm_unmap failed"
                ),
            }
        }
        match munmap_region(self.host, self.size) {
            Ok(()) => tracing::debug!(
                target: "ternvale::mem",
                host = format!("{:#x}", self.host.as_ptr() as usize),
                size = format!("{:#x}", self.size),
                "released host mapping"
            ),
            Err(error) => tracing::error!(
                target: "ternvale::mem",
                error = %error,
                "munmap failed"
            ),
        }
    }
}

/// Host RAM registered at guest physical addresses.
///
/// [`GuestMemory::add_region`] allocates pages the VMM can read and write.
/// [`GuestMemory::map`] does that and calls `hv_vm_map` with read, write, and
/// execute permission. Regions must not overlap.
pub struct GuestMemory {
    regions: Vec<Region>,
}

// SAFETY: host mappings are exclusive to this VMM. Callers share `GuestMemory`
// only behind a `Mutex`, and every access goes through the checked read/write
// helpers. The Hypervisor map is process-wide and not tied to a thread.
unsafe impl Send for GuestMemory {}
unsafe impl Sync for GuestMemory {}

impl std::fmt::Debug for GuestMemory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GuestMemory")
            .field("regions", &self.regions.len())
            .finish()
    }
}

impl GuestMemory {
    /// Check the host page size and return an empty GPA map.
    #[tracing::instrument(level = "debug", target = "ternvale::mem", skip_all)]
    pub fn new() -> Result<Self, MemoryError> {
        let bytes = host_page_size()?;
        tracing::info!(target: "ternvale::mem", host_page_size = bytes, "host page size");
        if bytes != HOST_PAGE_SIZE {
            tracing::error!(
                target: "ternvale::mem",
                host_page_size = bytes,
                expected = HOST_PAGE_SIZE,
                "host page size is not 16 KiB"
            );
            return Err(MemoryError::UnexpectedPageSize {
                bytes,
                expected: HOST_PAGE_SIZE,
            });
        }
        Ok(Self {
            regions: Vec::new(),
        })
    }

    /// Allocate anonymous zeroed memory and register it at `gpa`.
    ///
    /// Does not call the hypervisor. The pages are still released on drop.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::mem",
        skip_all,
        fields(gpa = format!("{:#x}", gpa), size = format!("{:#x}", size))
    )]
    pub fn add_region(&mut self, gpa: u64, size: u64) -> Result<(), MemoryError> {
        let region = self.alloc_region(gpa, size)?;
        let host = region.host.as_ptr() as usize;
        self.insert(region);
        tracing::info!(
            target: "ternvale::mem",
            gpa = format!("{:#x}", gpa),
            size = format!("{:#x}", size),
            host = format!("{:#x}", host),
            "allocated guest region"
        );
        Ok(())
    }

    /// Allocate anonymous zeroed memory and map it into `vm` at `gpa`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::mem",
        skip_all,
        fields(gpa = format!("{:#x}", gpa), size = format!("{:#x}", size))
    )]
    pub fn map(&mut self, vm: &ternvale_hv::Vm, gpa: u64, size: u64) -> Result<(), MemoryError> {
        tracing::debug!(target: "ternvale::mem", vm = ?vm, "mapping region into the guest");
        let mut region = self.alloc_region(gpa, size)?;
        let bytes = region.size;
        if let Err(source) =
            ternvale_hv::map_memory(region.host, gpa, bytes, ternvale_hv::HV_MEMORY_RWX)
        {
            let error = MemoryError::Map { gpa, size, source };
            tracing::error!(target: "ternvale::mem", error = %error, "hv_vm_map failed");
            return Err(error);
        }
        region.guest_mapped = true;
        let host = region.host.as_ptr() as usize;
        self.insert(region);
        tracing::info!(
            target: "ternvale::mem",
            gpa = format!("{:#x}", gpa),
            size = format!("{:#x}", size),
            host = format!("{:#x}", host),
            flags = format!("{:#x}", ternvale_hv::HV_MEMORY_RWX),
            "mapped guest region"
        );
        Ok(())
    }

    /// Read `dst.len()` bytes at `gpa`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::mem",
        skip_all,
        fields(gpa = format!("{:#x}", gpa), size = dst.len())
    )]
    pub fn read_bytes(&self, gpa: u64, dst: &mut [u8]) -> Result<(), MemoryError> {
        if dst.is_empty() {
            return Ok(());
        }
        let region = self.access(gpa, dst.len(), "read")?;
        let offset = (gpa - region.gpa) as usize;
        // SAFETY: `access` proved `offset + dst.len() <= region.size`. `host`
        // is an anonymous mapping of `region.size` bytes that lives until drop.
        unsafe {
            std::ptr::copy_nonoverlapping(
                region.host.as_ptr().add(offset),
                dst.as_mut_ptr(),
                dst.len(),
            );
        }
        Ok(())
    }

    /// Write `src` at `gpa`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::mem",
        skip_all,
        fields(gpa = format!("{:#x}", gpa), size = src.len())
    )]
    pub fn write_bytes(&mut self, gpa: u64, src: &[u8]) -> Result<(), MemoryError> {
        if src.is_empty() {
            return Ok(());
        }
        let region = self.access(gpa, src.len(), "write")?;
        let offset = (gpa - region.gpa) as usize;
        // SAFETY: `access` proved `offset + src.len() <= region.size`. `host`
        // is an anonymous mapping of `region.size` bytes that lives until drop.
        unsafe {
            std::ptr::copy_nonoverlapping(
                src.as_ptr(),
                region.host.as_ptr().add(offset),
                src.len(),
            );
        }
        Ok(())
    }

    guest_int!(read_u8, write_u8, u8);
    guest_int!(read_u16, write_u16, u16);
    guest_int!(read_u32, write_u32, u32);
    guest_int!(read_u64, write_u64, u64);

    fn alloc_region(&mut self, gpa: u64, size: u64) -> Result<Region, MemoryError> {
        let bytes = self.reserve(gpa, size)?;
        let host = mmap_anonymous(bytes)?;
        Ok(Region {
            gpa,
            size: bytes,
            host,
            guest_mapped: false,
        })
    }

    fn reserve(&self, gpa: u64, size: u64) -> Result<usize, MemoryError> {
        let bytes = check_layout(gpa, size).map_err(reject_region)?;
        if let Some((other_gpa, other_size)) = self.overlap(gpa, size) {
            return Err(reject_region(MemoryError::Overlap {
                gpa,
                size,
                other_gpa,
                other_size,
            }));
        }
        Ok(bytes)
    }

    fn overlap(&self, gpa: u64, size: u64) -> Option<(u64, u64)> {
        let end = gpa + size;
        self.regions.iter().find_map(|region| {
            let other_size = region.size as u64;
            if gpa < region.end() && region.gpa < end {
                Some((region.gpa, other_size))
            } else {
                None
            }
        })
    }

    fn insert(&mut self, region: Region) {
        let index = self
            .regions
            .partition_point(|existing| existing.gpa < region.gpa);
        self.regions.insert(index, region);
    }

    fn access(&self, gpa: u64, size: usize, op: &'static str) -> Result<&Region, MemoryError> {
        match self.find(gpa, size) {
            Ok(index) => Ok(&self.regions[index]),
            Err(error) => {
                tracing::warn!(
                    target: "ternvale::mem",
                    gpa = format!("{:#x}", gpa),
                    size,
                    op,
                    error = %error,
                    "rejected guest memory access"
                );
                Err(error)
            }
        }
    }

    fn find(&self, gpa: u64, size: usize) -> Result<usize, MemoryError> {
        let len = u64::try_from(size).map_err(|_| MemoryError::OutOfRange { gpa, size })?;
        let Some(end) = gpa.checked_add(len) else {
            return Err(MemoryError::OutOfRange { gpa, size });
        };
        let mut matched = None;
        for (index, region) in self.regions.iter().enumerate() {
            if gpa < region.end() && region.gpa < end {
                if matched.is_some() {
                    return Err(MemoryError::CrossRegion { gpa, size });
                }
                matched = Some(index);
            }
        }
        let Some(index) = matched else {
            return Err(MemoryError::OutOfRange { gpa, size });
        };
        let region = &self.regions[index];
        if gpa < region.gpa || end > region.end() {
            return Err(MemoryError::OutOfRange { gpa, size });
        }
        Ok(index)
    }
}

fn reject_region(error: MemoryError) -> MemoryError {
    tracing::warn!(
        target: "ternvale::mem",
        error = %error,
        "rejected guest memory region"
    );
    error
}

fn check_layout(gpa: u64, size: u64) -> Result<usize, MemoryError> {
    if gpa % HOST_PAGE_SIZE != 0 {
        return Err(MemoryError::MisalignedGpa {
            gpa,
            align: HOST_PAGE_SIZE,
        });
    }
    if size == 0 || size % HOST_PAGE_SIZE != 0 {
        return Err(MemoryError::MisalignedSize {
            size,
            align: HOST_PAGE_SIZE,
        });
    }
    if gpa.checked_add(size).is_none() {
        return Err(MemoryError::Overflow { gpa, size });
    }
    usize::try_from(size).map_err(|_| MemoryError::MisalignedSize {
        size,
        align: HOST_PAGE_SIZE,
    })
}

#[cfg(test)]
mod tests;
