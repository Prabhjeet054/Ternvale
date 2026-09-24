//! Failures while allocating, mapping, or touching guest physical memory.

use thiserror::Error;

/// A guest-memory operation was rejected or a host call failed.
#[derive(Debug, Error)]
pub enum MemoryError {
    /// Guest physical address was not a multiple of the host page size.
    #[error("gpa {gpa:#x} is not aligned to {align} bytes")]
    MisalignedGpa {
        /// Rejected guest physical address.
        gpa: u64,
        /// Required alignment, in bytes.
        align: u64,
    },

    /// Size was zero or not a multiple of the host page size.
    #[error("size {size:#x} must be a positive multiple of {align} bytes")]
    MisalignedSize {
        /// Rejected length, in bytes.
        size: u64,
        /// Required alignment, in bytes.
        align: u64,
    },

    /// `mmap` returned an address that is not page aligned.
    #[error("host address {addr:#x} is not aligned to {align} bytes")]
    MisalignedHost {
        /// Host pointer value.
        addr: u64,
        /// Required alignment, in bytes.
        align: u64,
    },

    /// `gpa + size` does not fit in a `u64`.
    #[error("guest region {gpa:#x} size {size:#x} overflows")]
    Overflow {
        /// Start of the rejected region.
        gpa: u64,
        /// Length of the rejected region.
        size: u64,
    },

    /// The new region shares bytes with one already registered.
    #[error("region {gpa:#x} size {size:#x} overlaps {other_gpa:#x} size {other_size:#x}")]
    Overlap {
        /// Start of the rejected region.
        gpa: u64,
        /// Length of the rejected region.
        size: u64,
        /// Start of the region already registered.
        other_gpa: u64,
        /// Length of the region already registered.
        other_size: u64,
    },

    /// Anonymous `mmap` failed.
    #[error("mmap {size:#x} bytes: {source}")]
    Mmap {
        /// Requested length.
        size: u64,
        /// OS error from `mmap`.
        source: std::io::Error,
    },

    /// `munmap` failed.
    #[error("munmap {addr:#x} size {size:#x}: {source}")]
    Munmap {
        /// Host address passed to `munmap`.
        addr: u64,
        /// Length passed to `munmap`.
        size: u64,
        /// OS error from `munmap`.
        source: std::io::Error,
    },

    /// `sysconf(_SC_PAGESIZE)` failed.
    #[error("sysconf(_SC_PAGESIZE) failed")]
    PageSize,

    /// The host page size is not 16 KiB.
    #[error("host page size is {bytes}, expected {expected}")]
    UnexpectedPageSize {
        /// Value returned by `sysconf`.
        bytes: u64,
        /// Page size this VMM maps with.
        expected: u64,
    },

    /// `hv_vm_map` failed.
    #[error("hv_vm_map gpa {gpa:#x} size {size:#x}: {source}")]
    Map {
        /// Guest physical address.
        gpa: u64,
        /// Length passed to `hv_vm_map`.
        size: u64,
        /// Framework error.
        source: ternvale_hv::HvError,
    },

    /// `hv_vm_unmap` failed.
    #[error("hv_vm_unmap gpa {gpa:#x} size {size:#x}: {source}")]
    Unmap {
        /// Guest physical address.
        gpa: u64,
        /// Length passed to `hv_vm_unmap`.
        size: u64,
        /// Framework error.
        source: ternvale_hv::HvError,
    },

    /// The access is not fully inside one registered region.
    #[error("guest access {gpa:#x} size {size} is outside mapped memory")]
    OutOfRange {
        /// Guest physical address of the access.
        gpa: u64,
        /// Access length, in bytes.
        size: usize,
    },

    /// The access touches more than one region.
    #[error("guest access {gpa:#x} size {size} crosses a region boundary")]
    CrossRegion {
        /// Guest physical address of the access.
        gpa: u64,
        /// Access length, in bytes.
        size: usize,
    },
}
