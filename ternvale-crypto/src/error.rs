//! Typed crypto failures.

use std::path::PathBuf;

/// At-rest encryption failure.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// `getrandom` failed.
    #[error("could not read OS random bytes for a key or nonce")]
    Rng,
    /// Key length is not what AES-256 / HKDF expected.
    #[error("encryption key is {found} bytes; expected {expected}")]
    BadKeyLen {
        /// Required length.
        expected: usize,
        /// Actual length.
        found: usize,
    },
    /// Wrapped-key or GCM envelope magic did not match.
    #[error("bad encryption magic (expected {expected}){}", path.as_ref().map(|p| format!(" in {}", p.display())).unwrap_or_default())]
    BadMagic {
        /// Optional path.
        path: Option<PathBuf>,
        /// Expected label.
        expected: &'static str,
    },
    /// Version field is not supported.
    #[error("unsupported {kind} encryption version {found} (supported: {supported})")]
    UnsupportedVersion {
        /// File kind.
        kind: &'static str,
        /// Version on disk.
        found: u32,
        /// Version this build writes.
        supported: u32,
    },
    /// Truncated header or ciphertext.
    #[error("truncated encrypted {what}")]
    Truncated {
        /// What was being parsed.
        what: &'static str,
    },
    /// AES-GCM authentication failed (wrong key or tampering).
    #[error("authentication failed ({what}): wrong key or the file was modified")]
    Auth {
        /// Envelope role (`dek`, `nvram`, `tpm`, `snapshot`, …).
        what: &'static str,
    },
    /// Hex from Keychain or `set-data-key` is not valid.
    #[error("encryption key hex is not valid")]
    BadHex,
    /// Disk I/O that is not sector-aligned for XTS.
    #[error("XTS I/O must be a multiple of 512 bytes (offset {offset}, len {len})")]
    Unaligned {
        /// File offset.
        offset: u64,
        /// Buffer length.
        len: usize,
    },
    /// Envelope claimed more chunks or bytes than we will allocate.
    #[error("encrypted {what} exceeds the size limit")]
    TooLarge {
        /// Envelope role.
        what: &'static str,
    },
    /// Host I/O on an encrypted file.
    #[error("encrypted file I/O on {path}: {source}", path = path.display())]
    Io {
        /// Path.
        path: PathBuf,
        /// OS error.
        source: std::io::Error,
    },
}

impl CryptoError {
    /// True when a wrong key or a flipped ciphertext byte caused the failure.
    #[must_use]
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::Auth { .. })
    }
}
