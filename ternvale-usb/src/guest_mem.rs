//! Guest physical memory access for TRB rings (no VMM dependency).

/// Read/write guest RAM for xhci rings.
pub trait GuestMemoryAccess: Send {
    /// Read `buf.len()` bytes from `gpa`.
    fn read_bytes(&self, gpa: u64, buf: &mut [u8]) -> Result<(), MemError>;
    /// Write `data` to `gpa`.
    fn write_bytes(&mut self, gpa: u64, data: &[u8]) -> Result<(), MemError>;
}

/// Guest memory access failed.
#[derive(Debug, thiserror::Error)]
pub enum MemError {
    /// Read failed.
    #[error("guest read at {gpa:#x}: {message}")]
    Read {
        /// Address.
        gpa: u64,
        /// Why.
        message: String,
    },
    /// Write failed.
    #[error("guest write at {gpa:#x}: {message}")]
    Write {
        /// Address.
        gpa: u64,
        /// Why.
        message: String,
    },
    /// TRB bytes were invalid.
    #[error("malformed trb at {gpa:#x}")]
    BadTrb {
        /// Address.
        gpa: u64,
    },
}
