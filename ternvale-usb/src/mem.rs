//! Guest-memory helpers for TRB rings.

use crate::guest_mem::{GuestMemoryAccess, MemError};
use crate::trb::{Trb, TRB_SIZE};

/// Read one TRB from guest physical `gpa`.
#[tracing::instrument(level = "trace", target = "ternvale::usb", skip(mem), fields(gpa = format!("{gpa:#x}")))]
pub fn read_trb(mem: &dyn GuestMemoryAccess, gpa: u64) -> Result<Trb, MemError> {
    let mut buf = [0u8; TRB_SIZE];
    mem.read_bytes(gpa, &mut buf)?;
    Trb::from_bytes(&buf).ok_or(MemError::BadTrb { gpa })
}

/// Write one TRB to guest physical `gpa`.
#[tracing::instrument(level = "trace", target = "ternvale::usb", skip(mem, trb), fields(gpa = format!("{gpa:#x}")))]
pub fn write_trb(mem: &mut dyn GuestMemoryAccess, gpa: u64, trb: Trb) -> Result<(), MemError> {
    mem.write_bytes(gpa, &trb.to_bytes())
}

/// Read `len` bytes from guest memory.
#[tracing::instrument(level = "trace", target = "ternvale::usb", skip(mem), fields(gpa = format!("{gpa:#x}"), len))]
pub fn read_bytes(mem: &dyn GuestMemoryAccess, gpa: u64, len: usize) -> Result<Vec<u8>, MemError> {
    let mut buf = vec![0u8; len];
    mem.read_bytes(gpa, &mut buf)?;
    Ok(buf)
}

/// Write bytes to guest memory.
#[tracing::instrument(level = "trace", target = "ternvale::usb", skip(mem, data), fields(gpa = format!("{gpa:#x}"), len = data.len()))]
pub fn write_bytes(mem: &mut dyn GuestMemoryAccess, gpa: u64, data: &[u8]) -> Result<(), MemError> {
    mem.write_bytes(gpa, data)
}
