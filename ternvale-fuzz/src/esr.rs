//! ESR_EL2 decoder.

use ternvale_vmm::decode_esr;

use crate::quiet;

/// Decode a random ESR and GPA.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let mut raw = [0u8; 16];
    let n = data.len().min(16);
    raw[..n].copy_from_slice(&data[..n]);
    let _ = decode_esr(u64_le(&raw[0..8]), u64_le(&raw[8..16]));
}

fn u64_le(bytes: &[u8]) -> u64 {
    let mut raw = [0u8; 8];
    let n = bytes.len().min(8);
    raw[..n].copy_from_slice(&bytes[..n]);
    u64::from_le_bytes(raw)
}
