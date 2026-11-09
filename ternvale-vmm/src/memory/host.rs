//! Anonymous host mappings for guest RAM.
//!
//! `hv_vm.h` says the host address, IPA, and size must be page aligned. Apple
//! Silicon's host page is 16 KiB. [`host_page_size`] reads `sysconf` so a
//! mismatch is a typed error instead of a bad `hv_vm_map`. Under Miri, pages
//! come from the global allocator so virtqueue tests can run without `mmap`.

use std::ptr::NonNull;

use super::error::MemoryError;

/// Apple Silicon host page size, in bytes.
pub const HOST_PAGE_SIZE: u64 = 16 * 1024;

/// `sysconf(_SC_PAGESIZE)`, or the Apple Silicon constant under Miri.
pub(super) fn host_page_size() -> Result<u64, MemoryError> {
    #[cfg(miri)]
    {
        Ok(HOST_PAGE_SIZE)
    }
    #[cfg(not(miri))]
    {
        // SAFETY: `sysconf(_SC_PAGESIZE)` takes no pointer and reads a process constant.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        u64::try_from(page).map_err(|_| {
            tracing::error!(target: "ternvale::mem", "sysconf(_SC_PAGESIZE) failed");
            MemoryError::PageSize
        })
    }
}

/// Reject a host address that `hv_vm_map` would not accept.
#[cfg_attr(miri, allow(dead_code))]
pub(super) fn require_host_alignment(addr: u64) -> Result<(), MemoryError> {
    if !addr.is_multiple_of(HOST_PAGE_SIZE) {
        return Err(MemoryError::MisalignedHost {
            addr,
            align: HOST_PAGE_SIZE,
        });
    }
    Ok(())
}

/// Anonymous, privately mapped, zero-filled pages.
pub(super) fn mmap_anonymous(size: usize) -> Result<NonNull<u8>, MemoryError> {
    #[cfg(miri)]
    {
        miri_alloc(size)
    }
    #[cfg(not(miri))]
    {
        mmap_anonymous_host(size)
    }
}

/// Release a mapping from [`mmap_anonymous`].
pub(super) fn munmap_region(host: NonNull<u8>, size: usize) -> Result<(), MemoryError> {
    #[cfg(miri)]
    {
        miri_dealloc(host, size)
    }
    #[cfg(not(miri))]
    {
        munmap_region_host(host, size)
    }
}

#[cfg(not(miri))]
fn mmap_anonymous_host(size: usize) -> Result<NonNull<u8>, MemoryError> {
    // SAFETY: a null address lets the kernel pick a page-aligned base.
    // `MAP_ANON | MAP_PRIVATE` with fd -1 is an anonymous mapping, which the
    // kernel zero-fills. `size` is a non-zero multiple of the host page.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_ANON | libc::MAP_PRIVATE,
            -1,
            0,
        )
    };
    if ptr.is_null() || ptr == libc::MAP_FAILED {
        let source = std::io::Error::last_os_error();
        tracing::error!(
            target: "ternvale::mem",
            size,
            error = %source,
            "mmap failed"
        );
        return Err(MemoryError::Mmap {
            size: size as u64,
            source,
        });
    }
    let addr = ptr as u64;
    if let Err(error) = require_host_alignment(addr) {
        // SAFETY: `ptr` was just returned by mmap for `size` bytes.
        let rc = unsafe { libc::munmap(ptr, size) };
        if rc != 0 {
            let source = std::io::Error::last_os_error();
            tracing::error!(
                target: "ternvale::mem",
                addr = format!("{:#x}", addr),
                error = %source,
                "munmap after misaligned mmap failed"
            );
        }
        tracing::warn!(
            target: "ternvale::mem",
            error = %error,
            "rejected guest memory region"
        );
        return Err(error);
    }
    // SAFETY: `ptr` was checked non-null and not `MAP_FAILED` (`MAP_FAILED` is -1).
    Ok(unsafe { NonNull::new_unchecked(ptr.cast()) })
}

#[cfg(not(miri))]
fn munmap_region_host(host: NonNull<u8>, size: usize) -> Result<(), MemoryError> {
    // SAFETY: `host` was returned by `mmap` for `size` bytes and is unmapped once.
    let rc = unsafe { libc::munmap(host.as_ptr().cast(), size) };
    if rc != 0 {
        let source = std::io::Error::last_os_error();
        return Err(MemoryError::Munmap {
            addr: host.as_ptr() as u64,
            size: size as u64,
            source,
        });
    }
    Ok(())
}

#[cfg(miri)]
fn miri_layout(size: usize) -> Result<std::alloc::Layout, MemoryError> {
    std::alloc::Layout::from_size_align(size, HOST_PAGE_SIZE as usize).map_err(|_| {
        MemoryError::MisalignedSize {
            size: size as u64,
            align: HOST_PAGE_SIZE,
        }
    })
}

#[cfg(miri)]
fn miri_alloc(size: usize) -> Result<NonNull<u8>, MemoryError> {
    let layout = miri_layout(size)?;
    // SAFETY: `layout` is a non-zero multiple of 16 KiB from [`check_layout`].
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    NonNull::new(ptr).ok_or_else(|| MemoryError::Mmap {
        size: size as u64,
        source: std::io::Error::from(std::io::ErrorKind::OutOfMemory),
    })
}

#[cfg(miri)]
fn miri_dealloc(host: NonNull<u8>, size: usize) -> Result<(), MemoryError> {
    let layout = miri_layout(size)?;
    // SAFETY: `host` came from [`miri_alloc`] with the same `size`.
    unsafe { std::alloc::dealloc(host.as_ptr(), layout) };
    Ok(())
}
