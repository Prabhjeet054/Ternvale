//! At-rest encryption for Ternvale.
//!
//! * Disk images and COW snapshot **layers**: AES-256-XTS per 512-byte sector
//!   (random-access). **XTS gives confidentiality, not integrity.**
//! * NVRAM, TPM state, and memory-snapshot files: AES-256-GCM (authenticated).
//! * One random 32-byte data key per VM, wrapped with a Keychain wrapping key.
//!   The unwrapped DEK reaches the VMM over the control socket, never argv/env.

mod context;
mod error;
mod gcm;
mod hex;
mod secret;
mod wrap;
mod xts;

pub use context::CryptoContext;
pub use error::CryptoError;
pub use gcm::{
    is_envelope, read_file as read_gcm_file, unwrap_file_in_place, wrap_file_in_place,
    write_file as write_gcm_file, GcmKey, DEFAULT_CHUNK, ENVELOPE_MAGIC, ENVELOPE_VERSION,
};
pub use hex::{decode as decode_hex, encode as encode_hex};
pub use secret::Secret;
pub use wrap::{
    load_dek, rewrap, store_dek, unwrap_dek, unwrap_dek_at, verify_dek, wrap_dek, DEK_LEN,
    WRAP_FILE_LEN, WRAP_MAGIC, WRAP_VERSION,
};
pub use xts::{covering_range, DiskXts, SECTOR};

/// AAD for NVRAM envelopes.
pub const AAD_NVRAM: &[u8] = b"nvram";
/// AAD for TPM state envelopes.
pub const AAD_TPM: &[u8] = b"tpm";
/// AAD for guest RAM in a VM snapshot bundle.
pub const AAD_SNAP_RAM: &[u8] = b"snap-ram";
/// AAD for GIC state in a VM snapshot bundle.
pub const AAD_SNAP_GIC: &[u8] = b"snap-gic";
/// AAD for vCPU JSON in a VM snapshot bundle.
pub const AAD_SNAP_VCPU: &[u8] = b"snap-vcpu";
/// AAD for device JSON in a VM snapshot bundle.
pub const AAD_SNAP_DEV: &[u8] = b"snap-dev";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-crypto");
    }
}

#[cfg(test)]
#[path = "rest_tests.rs"]
mod rest_tests;
