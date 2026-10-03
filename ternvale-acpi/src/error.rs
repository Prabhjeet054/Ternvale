//! Errors from building or writing ACPI tables.

/// An ACPI table could not be built, laid out, or written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcpiError {
    /// The tables do not fit in the reserved guest region.
    #[error("acpi tables need {need:#x} bytes at {base:#x}, region has {have:#x}")]
    RegionTooSmall {
        /// First byte of the region.
        base: u64,
        /// Bytes the tables need.
        need: u64,
        /// Bytes the region has.
        have: u64,
    },
    /// The region base is not 16-byte aligned.
    #[error("acpi region base {base:#x} is not 16-byte aligned")]
    Misaligned {
        /// Rejected base.
        base: u64,
    },
    /// A table address would pass the end of the 64-bit address space.
    #[error("acpi tables at {base:#x} overflow the address space")]
    AddressOverflow {
        /// Region base.
        base: u64,
    },
    /// A table is longer than its 32-bit length field can say.
    #[error("acpi table {signature} is {len} bytes, more than a u32 length")]
    TableTooLarge {
        /// Table signature.
        signature: String,
        /// Byte length.
        len: usize,
    },
    /// A byte slice is shorter than the structure it should hold.
    #[error("{what} needs {need} bytes, got {len}")]
    Truncated {
        /// Structure name.
        what: &'static str,
        /// Bytes given.
        len: usize,
        /// Bytes needed.
        need: usize,
    },
    /// An AML package is longer than a PkgLength can encode (2^28 - 1).
    #[error("aml package body of {len} bytes is longer than PkgLength allows")]
    PkgLengthTooLarge {
        /// Body length.
        len: usize,
    },
    /// An AML name segment is not 1 to 4 characters of `A-Z`, `0-9`, `_`
    /// with a non-digit first character.
    #[error("aml name segment {name:?} is invalid")]
    BadNameSeg {
        /// Rejected segment.
        name: String,
    },
    /// An AML name path is malformed.
    #[error("aml name path {path:?}: {reason}")]
    BadNamePath {
        /// Rejected path.
        path: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The caller's guest-memory reader failed.
    #[error("read {what} at {gpa:#x}: {reason}")]
    Read {
        /// What was being read (`RSDP`, `table header`, a signature).
        what: String,
        /// Guest physical address.
        gpa: u64,
        /// Reader error text.
        reason: String,
    },
    /// A table read back from guest memory is malformed.
    #[error("bad {what} at {gpa:#x}: {reason}")]
    BadTable {
        /// Table or structure name.
        what: String,
        /// Guest physical address.
        gpa: u64,
        /// What is wrong with it.
        reason: String,
    },
    /// No RSDP with valid checksums in a memory image.
    #[error("no valid RSDP in {len:#x} bytes from {base:#x}")]
    NoRsdp {
        /// Guest physical address of the image's first byte.
        base: u64,
        /// Image length.
        len: u64,
    },
    /// The caller's guest-memory writer failed.
    #[error("write acpi table {signature} at {gpa:#x}: {reason}")]
    Write {
        /// Table signature.
        signature: String,
        /// Guest physical address of the table.
        gpa: u64,
        /// Writer error text.
        reason: String,
    },
}
