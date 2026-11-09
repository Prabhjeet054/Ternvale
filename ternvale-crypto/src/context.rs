//! Per-VM data key and the XTS / GCM ciphers derived from it.

use hkdf::Hkdf;
use sha2::Sha256;

use crate::{CryptoError, DiskXts, GcmKey, Secret};

const SALT: &[u8] = b"ternvale-crypto-v1";
const XTS_INFO: &[u8] = b"xts-256";
const GCM_INFO: &[u8] = b"gcm-256";

/// One VM's at-rest encryption: a 32-byte DEK plus derived ciphers.
pub struct CryptoContext {
    dek: Secret,
    xts: DiskXts,
    gcm: GcmKey,
}

impl std::fmt::Debug for CryptoContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CryptoContext([redacted])")
    }
}

impl CryptoContext {
    /// Derive XTS and GCM keys from a 32-byte data-encryption key.
    #[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all)]
    pub fn from_dek(dek: Secret) -> Result<Self, CryptoError> {
        if dek.len() != 32 {
            return Err(CryptoError::BadKeyLen {
                expected: 32,
                found: dek.len(),
            });
        }
        let hk = Hkdf::<Sha256>::new(Some(SALT), dek.expose());
        let mut xts_bytes = vec![0u8; 64];
        hk.expand(XTS_INFO, &mut xts_bytes)
            .map_err(|_| CryptoError::BadKeyLen {
                expected: 64,
                found: 0,
            })?;
        let mut gcm_bytes = vec![0u8; 32];
        hk.expand(GCM_INFO, &mut gcm_bytes)
            .map_err(|_| CryptoError::BadKeyLen {
                expected: 32,
                found: 0,
            })?;
        let xts = DiskXts::from_key(&Secret::new(xts_bytes))?;
        let gcm = GcmKey::from_key(&Secret::new(gcm_bytes))?;
        tracing::info!(
            target: "ternvale::crypto",
            "data key accepted; XTS (disks/layers) and GCM (nvram/tpm/snapshots) ready"
        );
        Ok(Self { dek, xts, gcm })
    }

    /// New random DEK.
    #[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all)]
    pub fn generate() -> Result<Self, CryptoError> {
        Self::from_dek(Secret::random_32()?)
    }

    /// The 32-byte DEK (for wrapping / control-socket delivery). Never log it.
    pub fn dek(&self) -> &Secret {
        &self.dek
    }

    /// AES-256-XTS for disk and COW layer payloads.
    pub fn xts(&self) -> &DiskXts {
        &self.xts
    }

    /// AES-256-GCM for NVRAM, TPM state, and memory snapshots.
    pub fn gcm(&self) -> &GcmKey {
        &self.gcm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_dek_derives_same_ciphers() {
        let dek = Secret::new(vec![5u8; 32]);
        let a = CryptoContext::from_dek(dek.clone()).expect("a");
        let b = CryptoContext::from_dek(dek).expect("b");
        let mut x = vec![0u8; 512];
        x[0] = 1;
        let mut y = x.clone();
        a.xts().crypt(&mut x, 0, true).expect("enc a");
        b.xts().crypt(&mut y, 0, true).expect("enc b");
        assert_eq!(x, y);
        assert_eq!(format!("{:?}", a), "CryptoContext([redacted])");
    }
}
