//! Wrapped per-VM data key (`TVKW`). Rotation re-wraps without touching data.

use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};

use crate::{CryptoError, Secret};

/// Magic `TVKW` (Ternvale key wrap).
pub const WRAP_MAGIC: [u8; 4] = *b"TVKW";
/// Wrapped-key file version.
pub const WRAP_VERSION: u32 = 1;
/// Data-encryption key length (HKDF input).
pub const DEK_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const WRAP_AAD: &[u8] = b"ternvale-wrap-v1";
const VERIFY_PT: &[u8] = b"ternvale-dek-v1";
const VERIFY_AAD: &[u8] = b"ternvale-verify-v1";

/// On-disk size of a version-1 wrap file.
pub const WRAP_FILE_LEN: usize =
    4 + 4 + NONCE_LEN + DEK_LEN + TAG_LEN + NONCE_LEN + VERIFY_PT.len() + TAG_LEN;

/// Wrap `dek` with `wrapping_key` (both 32 bytes).
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all)]
pub fn wrap_dek(wrapping_key: &Secret, dek: &Secret) -> Result<Vec<u8>, CryptoError> {
    let wrap_aes = gcm(wrapping_key)?;
    let dek_aes = gcm(dek)?;
    let dek_bytes = dek.to_array::<DEK_LEN>()?;
    let mut wrap_nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut wrap_nonce).map_err(|_| CryptoError::Rng)?;
    let wrapped = wrap_aes
        .encrypt(
            &Nonce::from(wrap_nonce),
            Payload {
                msg: &dek_bytes,
                aad: WRAP_AAD,
            },
        )
        .map_err(|_| CryptoError::Auth { what: "wrap" })?;
    let mut verify_nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut verify_nonce).map_err(|_| CryptoError::Rng)?;
    let verifier = dek_aes
        .encrypt(
            &Nonce::from(verify_nonce),
            Payload {
                msg: VERIFY_PT,
                aad: VERIFY_AAD,
            },
        )
        .map_err(|_| CryptoError::Auth { what: "verifier" })?;
    let mut out = Vec::with_capacity(WRAP_FILE_LEN);
    out.extend_from_slice(&WRAP_MAGIC);
    out.extend_from_slice(&WRAP_VERSION.to_le_bytes());
    out.extend_from_slice(&wrap_nonce);
    out.extend_from_slice(&wrapped);
    out.extend_from_slice(&verify_nonce);
    out.extend_from_slice(&verifier);
    tracing::debug!(
        target: "ternvale::crypto",
        bytes = out.len(),
        "data key wrapped"
    );
    Ok(out)
}

/// Unwrap a `TVKW` blob. Wrong wrapping key → [`CryptoError::Auth`].
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all)]
pub fn unwrap_dek(wrapping_key: &Secret, blob: &[u8]) -> Result<Secret, CryptoError> {
    unwrap_dek_at(wrapping_key, blob, None)
}

/// [`unwrap_dek`] with a path for magic errors.
pub fn unwrap_dek_at(
    wrapping_key: &Secret,
    blob: &[u8],
    path: Option<&Path>,
) -> Result<Secret, CryptoError> {
    if blob.len() < WRAP_FILE_LEN {
        return Err(CryptoError::Truncated {
            what: "wrapped-dek",
        });
    }
    if blob[0..4] != WRAP_MAGIC {
        return Err(CryptoError::BadMagic {
            path: path.map(Path::to_path_buf),
            expected: "TVKW",
        });
    }
    let version = u32::from_le_bytes(blob[4..8].try_into().expect("ver"));
    if version != WRAP_VERSION {
        return Err(CryptoError::UnsupportedVersion {
            kind: "wrapped-dek",
            found: version,
            supported: WRAP_VERSION,
        });
    }
    let wrap_nonce: [u8; NONCE_LEN] = blob[8..20].try_into().expect("wn");
    let wrapped = &blob[20..20 + DEK_LEN + TAG_LEN];
    let verify_nonce: [u8; NONCE_LEN] = blob
        [20 + DEK_LEN + TAG_LEN..20 + DEK_LEN + TAG_LEN + NONCE_LEN]
        .try_into()
        .expect("vn");
    let verifier = &blob[20 + DEK_LEN + TAG_LEN + NONCE_LEN..WRAP_FILE_LEN];
    let wrap_aes = gcm(wrapping_key)?;
    let dek_bytes = wrap_aes
        .decrypt(
            &Nonce::from(wrap_nonce),
            Payload {
                msg: wrapped,
                aad: WRAP_AAD,
            },
        )
        .map_err(|_| CryptoError::Auth { what: "unwrap" })?;
    let dek = Secret::new(dek_bytes);
    let dek_aes = gcm(&dek)?;
    let check = dek_aes
        .decrypt(
            &Nonce::from(verify_nonce),
            Payload {
                msg: verifier,
                aad: VERIFY_AAD,
            },
        )
        .map_err(|_| CryptoError::Auth { what: "verifier" })?;
    if check != VERIFY_PT {
        return Err(CryptoError::Auth { what: "verifier" });
    }
    Ok(dek)
}

/// Confirm `dek` matches the verifier in `blob` (wrong `set-data-key`).
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all)]
pub fn verify_dek(dek: &Secret, blob: &[u8]) -> Result<(), CryptoError> {
    if blob.len() < WRAP_FILE_LEN {
        return Err(CryptoError::Truncated {
            what: "wrapped-dek",
        });
    }
    let verify_nonce: [u8; NONCE_LEN] = blob
        [20 + DEK_LEN + TAG_LEN..20 + DEK_LEN + TAG_LEN + NONCE_LEN]
        .try_into()
        .expect("vn");
    let verifier = &blob[20 + DEK_LEN + TAG_LEN + NONCE_LEN..WRAP_FILE_LEN];
    let dek_aes = gcm(dek)?;
    let check = dek_aes
        .decrypt(
            &Nonce::from(verify_nonce),
            Payload {
                msg: verifier,
                aad: VERIFY_AAD,
            },
        )
        .map_err(|_| CryptoError::Auth { what: "data-key" })?;
    if check != VERIFY_PT {
        return Err(CryptoError::Auth { what: "data-key" });
    }
    Ok(())
}

/// New wrapping of the same DEK (key rotation; data files unchanged).
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all)]
pub fn rewrap(
    old_wrapping: &Secret,
    new_wrapping: &Secret,
    blob: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let dek = unwrap_dek(old_wrapping, blob)?;
    wrap_dek(new_wrapping, &dek)
}

/// Read `path`, unwrap, return the DEK.
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all, fields(path = %path.display()))]
pub fn load_dek(wrapping_key: &Secret, path: &Path) -> Result<Secret, CryptoError> {
    let blob = std::fs::read(path).map_err(|source| CryptoError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    unwrap_dek_at(wrapping_key, &blob, Some(path))
}

/// Wrap `dek` and write `path`.
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all, fields(path = %path.display()))]
pub fn store_dek(wrapping_key: &Secret, dek: &Secret, path: &Path) -> Result<(), CryptoError> {
    let blob = wrap_dek(wrapping_key, dek)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CryptoError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(path, blob).map_err(|source| CryptoError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    tracing::info!(
        target: "ternvale::crypto",
        path = %path.display(),
        "wrote wrapped data key"
    );
    Ok(())
}

fn gcm(key: &Secret) -> Result<Aes256Gcm, CryptoError> {
    let bytes = key.expose();
    if bytes.len() != 32 {
        return Err(CryptoError::BadKeyLen {
            expected: 32,
            found: bytes.len(),
        });
    }
    Aes256Gcm::new_from_slice(bytes).map_err(|_| CryptoError::BadKeyLen {
        expected: 32,
        found: bytes.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_unwrap_wrong_key_and_rotate() {
        let wrap = Secret::new(vec![1u8; 32]);
        let dek = Secret::new(vec![2u8; 32]);
        let blob = wrap_dek(&wrap, &dek).expect("wrap");
        assert_eq!(blob.len(), WRAP_FILE_LEN);
        let got = unwrap_dek(&wrap, &blob).expect("unwrap");
        assert_eq!(got.expose(), dek.expose());
        let wrong = Secret::new(vec![9u8; 32]);
        assert!(unwrap_dek(&wrong, &blob).expect_err("wrong").is_auth());
        verify_dek(&dek, &blob).expect("verify");
        assert!(verify_dek(&wrong, &blob).expect_err("bad dek").is_auth());
        let new_wrap = Secret::new(vec![3u8; 32]);
        let rotated = rewrap(&wrap, &new_wrap, &blob).expect("rewrap");
        assert_eq!(
            unwrap_dek(&new_wrap, &rotated).expect("new").expose(),
            dek.expose()
        );
        assert!(unwrap_dek(&wrap, &rotated).is_err());
    }
}
