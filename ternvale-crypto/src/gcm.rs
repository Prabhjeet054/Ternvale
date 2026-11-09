//! AES-256-GCM envelopes for NVRAM, TPM state, and memory-snapshot files.

use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use zeroize::Zeroize;

use crate::{CryptoError, Secret};

/// Magic `TVGE` (Ternvale GCM envelope).
pub const ENVELOPE_MAGIC: [u8; 4] = *b"TVGE";
/// Envelope version.
pub const ENVELOPE_VERSION: u32 = 1;
/// Default plaintext chunk size (1 MiB).
pub const DEFAULT_CHUNK: u32 = 1 << 20;
const HEADER: usize = 20;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const MAX_CHUNKS: u64 = 1 << 20;
const MAX_PLAIN: u64 = 64 << 30;

/// AES-256-GCM key (32 bytes).
#[derive(Clone)]
pub struct GcmKey {
    cipher: Aes256Gcm,
}

impl std::fmt::Debug for GcmKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GcmKey([redacted])")
    }
}

impl GcmKey {
    /// Build from a 32-byte key.
    #[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all)]
    pub fn from_key(key: &Secret) -> Result<Self, CryptoError> {
        let bytes = key.expose();
        if bytes.len() != 32 {
            return Err(CryptoError::BadKeyLen {
                expected: 32,
                found: bytes.len(),
            });
        }
        Ok(Self {
            cipher: Aes256Gcm::new_from_slice(bytes).map_err(|_| CryptoError::BadKeyLen {
                expected: 32,
                found: bytes.len(),
            })?,
        })
    }

    /// Seal `plain` with `aad` (purpose string, never a secret).
    #[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all, fields(plain_len = plain.len(), aad))]
    pub fn seal(&self, plain: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if plain.len() as u64 > MAX_PLAIN {
            return Err(CryptoError::TooLarge { what: "payload" });
        }
        let chunk = DEFAULT_CHUNK as usize;
        let nchunks = if plain.is_empty() {
            0u64
        } else {
            (plain.len() as u64).div_ceil(chunk as u64)
        };
        if nchunks > MAX_CHUNKS {
            return Err(CryptoError::TooLarge { what: "payload" });
        }
        let mut out =
            Vec::with_capacity(HEADER + plain.len() + nchunks as usize * (NONCE_LEN + 4 + TAG_LEN));
        out.extend_from_slice(&ENVELOPE_MAGIC);
        out.extend_from_slice(&ENVELOPE_VERSION.to_le_bytes());
        out.extend_from_slice(&DEFAULT_CHUNK.to_le_bytes());
        out.extend_from_slice(&nchunks.to_le_bytes());
        for (index, piece) in plain.chunks(chunk).enumerate() {
            let mut nonce_bytes = [0u8; NONCE_LEN];
            getrandom::getrandom(&mut nonce_bytes).map_err(|_| CryptoError::Rng)?;
            let nonce = Nonce::from(nonce_bytes);
            let mut chunk_aad = Vec::with_capacity(aad.len() + 8);
            chunk_aad.extend_from_slice(aad);
            chunk_aad.extend_from_slice(&(index as u64).to_le_bytes());
            let ct = self
                .cipher
                .encrypt(
                    &nonce,
                    Payload {
                        msg: piece,
                        aad: &chunk_aad,
                    },
                )
                .map_err(|_| CryptoError::Auth { what: "seal" })?;
            out.extend_from_slice(&nonce_bytes);
            out.extend_from_slice(&(ct.len() as u32).to_le_bytes());
            out.extend_from_slice(&ct);
            nonce_bytes.zeroize();
        }
        tracing::debug!(
            target: "ternvale::crypto",
            chunks = nchunks,
            out_len = out.len(),
            "GCM envelope sealed"
        );
        Ok(out)
    }

    /// Open an envelope. Rejects bad magic, version, or a failed tag.
    #[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all, fields(bytes = envelope.len(), aad))]
    pub fn open(&self, envelope: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.open_at(envelope, aad, None)
    }

    /// [`open`] with an optional path for `BadMagic`.
    pub fn open_at(
        &self,
        envelope: &[u8],
        aad: &[u8],
        path: Option<&Path>,
    ) -> Result<Vec<u8>, CryptoError> {
        if envelope.len() < HEADER {
            return Err(CryptoError::Truncated { what: "envelope" });
        }
        if envelope[0..4] != ENVELOPE_MAGIC {
            return Err(CryptoError::BadMagic {
                path: path.map(Path::to_path_buf),
                expected: "TVGE",
            });
        }
        let version = u32::from_le_bytes(envelope[4..8].try_into().expect("ver"));
        if version != ENVELOPE_VERSION {
            return Err(CryptoError::UnsupportedVersion {
                kind: "gcm-envelope",
                found: version,
                supported: ENVELOPE_VERSION,
            });
        }
        let chunk_size = u32::from_le_bytes(envelope[8..12].try_into().expect("cs"));
        let nchunks = u64::from_le_bytes(envelope[12..20].try_into().expect("n"));
        if chunk_size == 0 || nchunks > MAX_CHUNKS {
            return Err(CryptoError::TooLarge { what: "envelope" });
        }
        let mut pos = HEADER;
        let mut plain = Vec::new();
        for index in 0..nchunks {
            if envelope.len().saturating_sub(pos) < NONCE_LEN + 4 {
                return Err(CryptoError::Truncated { what: "chunk" });
            }
            let nonce_bytes: [u8; NONCE_LEN] =
                envelope[pos..pos + NONCE_LEN].try_into().expect("nonce");
            pos += NONCE_LEN;
            let clen =
                u32::from_le_bytes(envelope[pos..pos + 4].try_into().expect("clen")) as usize;
            pos += 4;
            if clen < TAG_LEN || envelope.len().saturating_sub(pos) < clen {
                return Err(CryptoError::Truncated { what: "ciphertext" });
            }
            if plain.len().saturating_add(clen) as u64 > MAX_PLAIN {
                return Err(CryptoError::TooLarge { what: "envelope" });
            }
            let ct = &envelope[pos..pos + clen];
            pos += clen;
            let mut chunk_aad = Vec::with_capacity(aad.len() + 8);
            chunk_aad.extend_from_slice(aad);
            chunk_aad.extend_from_slice(&index.to_le_bytes());
            let nonce = Nonce::from(nonce_bytes);
            let piece = self
                .cipher
                .decrypt(
                    &nonce,
                    Payload {
                        msg: ct,
                        aad: &chunk_aad,
                    },
                )
                .map_err(|_| CryptoError::Auth { what: "open" })?;
            plain.extend_from_slice(&piece);
        }
        if pos != envelope.len() {
            return Err(CryptoError::Truncated {
                what: "envelope-trailer",
            });
        }
        Ok(plain)
    }
}

/// True when `bytes` start with the GCM envelope magic.
#[must_use]
pub fn is_envelope(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[0..4] == ENVELOPE_MAGIC
}

/// Write `plain` as a GCM envelope at `path`.
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all, fields(path = %path.display(), aad, len = plain.len()))]
pub fn write_file(key: &GcmKey, path: &Path, plain: &[u8], aad: &[u8]) -> Result<(), CryptoError> {
    let sealed = key.seal(plain, aad)?;
    std::fs::write(path, sealed).map_err(|source| CryptoError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    tracing::info!(
        target: "ternvale::crypto",
        path = %path.display(),
        "wrote GCM-protected file"
    );
    Ok(())
}

/// Read `path`: decrypt a GCM envelope, or return raw bytes if unencrypted.
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all, fields(path = %path.display(), aad))]
pub fn read_file(key: Option<&GcmKey>, path: &Path, aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let bytes = std::fs::read(path).map_err(|source| CryptoError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !is_envelope(&bytes) {
        return Ok(bytes);
    }
    let Some(key) = key else {
        tracing::warn!(
            target: "ternvale::crypto",
            path = %path.display(),
            "GCM envelope found but no data key is loaded"
        );
        return Err(CryptoError::Auth {
            what: "missing-key",
        });
    };
    key.open_at(&bytes, aad, Some(path))
}

/// If `path` is a GCM envelope, decrypt it in place (plaintext for swtpm).
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all, fields(path = %path.display(), aad))]
pub fn unwrap_file_in_place(key: &GcmKey, path: &Path, aad: &[u8]) -> Result<bool, CryptoError> {
    if !path.is_file() {
        return Ok(false);
    }
    let bytes = std::fs::read(path).map_err(|source| CryptoError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !is_envelope(&bytes) {
        return Ok(false);
    }
    let plain = key.open_at(&bytes, aad, Some(path))?;
    std::fs::write(path, plain).map_err(|source| CryptoError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    tracing::info!(
        target: "ternvale::crypto",
        path = %path.display(),
        "unwrapped GCM file for backend use"
    );
    Ok(true)
}

/// Replace `path` with a GCM envelope of its current contents.
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all, fields(path = %path.display(), aad))]
pub fn wrap_file_in_place(key: &GcmKey, path: &Path, aad: &[u8]) -> Result<(), CryptoError> {
    if !path.is_file() {
        return Ok(());
    }
    let plain = std::fs::read(path).map_err(|source| CryptoError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if is_envelope(&plain) {
        return Ok(());
    }
    write_file(key, path, &plain, aad)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> GcmKey {
        GcmKey::from_key(&Secret::new(vec![3u8; 32])).expect("gcm")
    }

    #[test]
    fn round_trip_and_tamper() {
        let gcm = key();
        let sealed = gcm.seal(b"hello-nvram", b"nvram").expect("seal");
        assert!(is_envelope(&sealed));
        assert_eq!(gcm.open(&sealed, b"nvram").expect("open"), b"hello-nvram");
        let mut bad = sealed.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        let err = gcm.open(&bad, b"nvram").expect_err("tamper");
        assert!(err.is_auth(), "{err}");
        assert!(gcm.open(&sealed, b"tpm").expect_err("aad").is_auth());
    }

    #[test]
    fn empty_plain() {
        let gcm = key();
        let sealed = gcm.seal(b"", b"x").expect("seal");
        assert_eq!(gcm.open(&sealed, b"x").expect("open"), b"");
    }
}
