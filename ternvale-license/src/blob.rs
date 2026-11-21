//! Versioned on-disk license (`TVLK` v1): JSON payload + Ed25519 signature.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::payload::LicensePayload;
use crate::LicenseError;

/// Magic.
pub const MAGIC: &[u8; 4] = b"TVLK";
/// Format version.
pub const VERSION: u32 = 1;
const SIG_LEN: usize = 64;

/// Verified license bytes + claims.
#[derive(Debug, Clone)]
pub struct LicenseBlob {
    /// Claims.
    pub payload: LicensePayload,
}

/// Parse and verify `bytes` with `public`.
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn verify(bytes: &[u8], public: &VerifyingKey) -> Result<LicenseBlob, LicenseError> {
    if bytes.len() < 8 + SIG_LEN {
        return Err(LicenseError::Version("license blob too short".into()));
    }
    if &bytes[0..4] != MAGIC {
        return Err(LicenseError::Version("license magic is not TVLK".into()));
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap_or([0; 4]));
    if version != VERSION {
        return Err(LicenseError::Version(format!(
            "unsupported license version {version} (want {VERSION})"
        )));
    }
    let rest = &bytes[8..];
    if rest.len() < SIG_LEN {
        return Err(LicenseError::Version("license missing signature".into()));
    }
    let (json, sig_bytes) = rest.split_at(rest.len() - SIG_LEN);
    if json.len() > 8 * 1024 {
        return Err(LicenseError::Verify("license payload too large".into()));
    }
    let sig = Signature::from_bytes(
        sig_bytes
            .try_into()
            .map_err(|_| LicenseError::Verify("signature length".into()))?,
    );
    public
        .verify(json, &sig)
        .map_err(|_| LicenseError::Verify("license signature rejected".into()))?;
    let payload: LicensePayload = serde_json::from_slice(json)?;
    payload.validate()?;
    tracing::info!(
        target: "ternvale::license",
        id = %payload.id,
        edition = %payload.edition,
        seats = payload.seats,
        "license verified"
    );
    Ok(LicenseBlob { payload })
}

/// Sign `payload` (operator tool). Never log `signing`.
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn sign(payload: &LicensePayload, signing: &SigningKey) -> Result<Vec<u8>, LicenseError> {
    payload.validate()?;
    let json = serde_json::to_vec(payload)?;
    let sig = signing.sign(&json);
    let mut out = Vec::with_capacity(8 + json.len() + SIG_LEN);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(&sig.to_bytes());
    Ok(out)
}

/// Hostile bytes must not panic. Used by cargo-fuzz and the 10-minute soak.
#[tracing::instrument(level = "trace", target = "ternvale::license", skip_all, fields(len = data.len()))]
pub fn fuzz_verify(data: &[u8]) {
    let Ok(vk) = crate::pubkey::embedded_verifying_key() else {
        return;
    };
    match verify(data, &vk) {
        Ok(_) => {}
        Err(error) => {
            tracing::trace!(target: "ternvale::license", error = %error, "fuzz reject");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn payload() -> LicensePayload {
        LicensePayload {
            product: crate::payload::PRODUCT.into(),
            edition: "pro".into(),
            seats: 1,
            expiry_unix: 9_999_999_999,
            issued_unix: 1,
            id: "lic-test".into(),
        }
    }

    #[test]
    fn round_trip() {
        let sk = key();
        let blob = sign(&payload(), &sk).expect("sign");
        let got = verify(&blob, &sk.verifying_key()).expect("verify");
        assert_eq!(got.payload.id, "lic-test");
    }

    #[test]
    fn tampered_payload_fails() {
        let sk = key();
        let mut blob = sign(&payload(), &sk).expect("sign");
        let i = blob.len() - SIG_LEN - 1;
        blob[i] ^= 0x01;
        assert!(verify(&blob, &sk.verifying_key()).is_err());
    }

    #[test]
    fn wrong_product_fails() {
        let sk = key();
        let mut p = payload();
        p.product = "other".into();
        let blob = sign(&p, &sk).unwrap_or_else(|_| {
            let json = serde_json::to_vec(&p).expect("json");
            let sig = sk.sign(&json);
            let mut out = Vec::from(*MAGIC);
            out.extend_from_slice(&VERSION.to_le_bytes());
            out.extend_from_slice(&json);
            out.extend_from_slice(&sig.to_bytes());
            out
        });
        let err = verify(&blob, &sk.verifying_key()).expect_err("product");
        assert!(err.to_string().contains("wrong product"));
    }

    #[test]
    fn unknown_version_fails() {
        let mut bytes = Vec::from(*MAGIC);
        bytes.extend_from_slice(&99u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 64]);
        assert!(verify(&bytes, &key().verifying_key()).is_err());
    }
}
