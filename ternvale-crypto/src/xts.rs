//! AES-256-XTS for disk and snapshot *data* blocks.
//!
//! XTS is sector-addressable (tweak = 512-byte sector index) so virtio-blk and
//! COW layers can read or write any sector without decrypting the whole image.
//!
//! **XTS provides confidentiality, not integrity.** A guest or host that
//! tampers with ciphertext will see garbage plaintext; there is no
//! authentication tag. NVRAM, TPM state, and memory-snapshot files use
//! AES-256-GCM instead.
//!
//! All-zero sectors are left as zeros so sparse images stay sparse (an unused
//! hole is indistinguishable from a guest write of zeros).

use std::sync::Arc;

use aes::cipher::{generic_array::GenericArray, KeyInit};
use aes::Aes256;
use xts_mode::{get_tweak_default, Xts128};

use crate::{CryptoError, Secret};

/// Virtio-blk and raw images use 512-byte logical sectors.
pub const SECTOR: usize = 512;

/// Sector-aligned range covering `[offset, offset+len)`.
#[must_use]
pub fn covering_range(offset: u64, len: usize) -> (u64, usize) {
    let start = offset / SECTOR as u64 * SECTOR as u64;
    let end = offset.saturating_add(len as u64).div_ceil(SECTOR as u64) * SECTOR as u64;
    (start, (end - start) as usize)
}

/// AES-256-XTS (two 256-bit AES keys). `Clone` is cheap (`Arc`).
#[derive(Clone)]
pub struct DiskXts {
    inner: Arc<Xts128<Aes256>>,
}

impl std::fmt::Debug for DiskXts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DiskXts([redacted])")
    }
}

impl DiskXts {
    /// Build from a 64-byte XTS key (two AES-256 keys).
    #[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all)]
    pub fn from_key(key: &Secret) -> Result<Self, CryptoError> {
        let bytes = key.expose();
        if bytes.len() != 64 {
            return Err(CryptoError::BadKeyLen {
                expected: 64,
                found: bytes.len(),
            });
        }
        let cipher_1 = Aes256::new(GenericArray::from_slice(&bytes[..32]));
        let cipher_2 = Aes256::new(GenericArray::from_slice(&bytes[32..]));
        tracing::debug!(
            target: "ternvale::crypto",
            sector = SECTOR,
            "AES-256-XTS ready (confidentiality only; no integrity)"
        );
        Ok(Self {
            inner: Arc::new(Xts128::new(cipher_1, cipher_2)),
        })
    }

    /// Encrypt or decrypt `buf` at byte `offset` (both must be sector-aligned).
    /// All-zero sectors are left unchanged so sparse holes stay holes.
    #[tracing::instrument(level = "trace", target = "ternvale::crypto", skip_all, fields(offset, len = buf.len(), encrypt))]
    pub fn crypt(&self, buf: &mut [u8], offset: u64, encrypt: bool) -> Result<(), CryptoError> {
        if !offset.is_multiple_of(SECTOR as u64) || !buf.len().is_multiple_of(SECTOR) {
            return Err(CryptoError::Unaligned {
                offset,
                len: buf.len(),
            });
        }
        let first = offset / SECTOR as u64;
        let mut sector = 0u64;
        while sector * SECTOR as u64 + SECTOR as u64 <= buf.len() as u64 {
            let start = (sector as usize) * SECTOR;
            let slice = &mut buf[start..start + SECTOR];
            if slice.iter().all(|b| *b == 0) {
                sector += 1;
                continue;
            }
            let tweak_sector = first.saturating_add(sector);
            if encrypt {
                self.inner
                    .encrypt_area(slice, SECTOR, u128::from(tweak_sector), get_tweak_default);
            } else {
                self.inner
                    .decrypt_area(slice, SECTOR, u128::from(tweak_sector), get_tweak_default);
            }
            sector += 1;
        }
        Ok(())
    }

    /// Encrypt a copy of `plain` at `offset`.
    pub fn encrypt_to_vec(&self, plain: &[u8], offset: u64) -> Result<Vec<u8>, CryptoError> {
        let mut out = plain.to_vec();
        self.crypt(&mut out, offset, true)?;
        Ok(out)
    }

    /// Decrypt a copy of `cipher` at `offset`.
    pub fn decrypt_to_vec(&self, cipher: &[u8], offset: u64) -> Result<Vec<u8>, CryptoError> {
        let mut out = cipher.to_vec();
        self.crypt(&mut out, offset, false)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xts() -> DiskXts {
        DiskXts::from_key(&Secret::new(vec![7u8; 64])).expect("xts")
    }

    #[test]
    fn round_trip_and_sparse_zeros() {
        let xts = xts();
        let mut buf = vec![0u8; 512];
        buf[0] = 0xaa;
        let orig = buf.clone();
        xts.crypt(&mut buf, 512, true).expect("enc");
        assert_ne!(buf, orig);
        xts.crypt(&mut buf, 512, false).expect("dec");
        assert_eq!(buf, orig);
        let mut zeros = vec![0u8; 512];
        xts.crypt(&mut zeros, 0, true).expect("z");
        assert!(zeros.iter().all(|b| *b == 0));
    }

    #[test]
    fn debug_is_redacted() {
        assert_eq!(format!("{:?}", xts()), "DiskXts([redacted])");
    }
}
