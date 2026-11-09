//! PSCI dispatcher (`psci_call`).

use ternvale_vmm::psci_call;

use crate::quiet;

/// Dispatch a four-register SMCCC/PSCI call.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let mut raw = [0u8; 32];
    let n = data.len().min(32);
    raw[..n].copy_from_slice(&data[..n]);
    let _ = psci_call(word(&raw, 0), word(&raw, 8), word(&raw, 16), word(&raw, 24));
}

fn word(raw: &[u8], off: usize) -> u64 {
    let mut bytes = [0u8; 8];
    if raw.len() >= off + 8 {
        bytes.copy_from_slice(&raw[off..off + 8]);
    }
    u64::from_le_bytes(bytes)
}
