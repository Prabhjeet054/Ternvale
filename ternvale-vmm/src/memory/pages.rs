//! Page-aligned host buffers that live outside [`super::GuestMemory`].
//!
//! The variable-store flash maps and unmaps its backing pages as the guest
//! switches between read-array and command mode, so it owns them directly.

use std::ptr::NonNull;

use super::error::MemoryError;
use super::host::{mmap_anonymous, munmap_region, HOST_PAGE_SIZE};

/// Anonymous, zero-filled host pages. Released on drop; unmap any guest
/// mapping of them first.
pub struct HostPages {
    host: NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is exclusive to this value. The only aliasing is a guest
// read-only stage-2 mapping, which the owner tears down before dropping.
unsafe impl Send for HostPages {}

impl std::fmt::Debug for HostPages {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostPages")
            .field("host", &format_args!("{:#x}", self.host.as_ptr() as usize))
            .field("len", &format_args!("{:#x}", self.len))
            .finish()
    }
}

impl HostPages {
    /// Allocate `len` bytes. `len` must be a non-zero multiple of 16 KiB.
    #[tracing::instrument(level = "debug", target = "ternvale::mem", skip_all, fields(len = format!("{len:#x}")))]
    pub fn new(len: u64) -> Result<Self, MemoryError> {
        if len == 0 || len % HOST_PAGE_SIZE != 0 {
            tracing::warn!(target: "ternvale::mem", len = format!("{len:#x}"), "rejected host page buffer size");
            return Err(MemoryError::MisalignedSize {
                size: len,
                align: HOST_PAGE_SIZE,
            });
        }
        let bytes = usize::try_from(len).map_err(|_| MemoryError::MisalignedSize {
            size: len,
            align: HOST_PAGE_SIZE,
        })?;
        let host = mmap_anonymous(bytes)?;
        tracing::debug!(
            target: "ternvale::mem",
            host = format!("{:#x}", host.as_ptr() as usize),
            len = format!("{len:#x}"),
            "allocated host pages"
        );
        Ok(Self { host, len: bytes })
    }

    /// Byte length.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Base address, for `hv_vm_map`.
    pub fn host(&self) -> NonNull<u8> {
        self.host
    }

    /// The whole buffer.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `host` is a live mapping of `len` readable bytes owned by self.
        unsafe { std::slice::from_raw_parts(self.host.as_ptr(), self.len) }
    }

    /// The whole buffer, mutably.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `host` is a live mapping of `len` writable bytes, and
        // `&mut self` makes this the only host-side reference.
        unsafe { std::slice::from_raw_parts_mut(self.host.as_ptr(), self.len) }
    }
}

impl Drop for HostPages {
    fn drop(&mut self) {
        if let Err(error) = munmap_region(self.host, self.len) {
            tracing::error!(target: "ternvale::mem", error = %error, "munmap of host pages failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_zeroed_writable_pages_and_rejects_bad_sizes() {
        let mut pages = HostPages::new(2 * HOST_PAGE_SIZE).expect("alloc");
        assert_eq!(pages.len(), 2 * HOST_PAGE_SIZE as usize);
        assert!(pages.as_slice().iter().all(|&byte| byte == 0));
        pages.as_mut_slice()[5] = 0xab;
        assert_eq!(pages.as_slice()[5], 0xab);
        assert!(HostPages::new(0).is_err());
        assert!(HostPages::new(HOST_PAGE_SIZE + 1).is_err());
    }
}
