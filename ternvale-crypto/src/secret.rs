//! Key material that never appears in logs.

use std::fmt;

use zeroize::{Zeroize, ZeroizeOnDrop};

/// Byte secret: `Debug`/`Display` print `[redacted]`; dropped bytes are zeroed.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Secret(Vec<u8>);

impl Secret {
    /// Take ownership of `bytes`.
    #[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all)]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// 32 random bytes (wrapping key or data-encryption key).
    #[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all)]
    pub fn random_32() -> Result<Self, crate::CryptoError> {
        let mut bytes = vec![0u8; 32];
        getrandom::getrandom(&mut bytes).map_err(|_| crate::CryptoError::Rng)?;
        Ok(Self(bytes))
    }

    /// Borrow the bytes. Callers must not log the return value.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// Copy into a fixed array when `self` is exactly `N` bytes.
    pub fn to_array<const N: usize>(&self) -> Result<[u8; N], crate::CryptoError> {
        self.0
            .as_slice()
            .try_into()
            .map_err(|_| crate::CryptoError::BadKeyLen {
                expected: N,
                found: self.0.len(),
            })
    }

    /// Hex (lowercase) for Keychain / control-socket transport. Never log it.
    #[must_use]
    pub fn to_hex(&self) -> String {
        crate::hex::encode(&self.0)
    }

    /// Parse hex from Keychain or `set-data-key`.
    #[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all)]
    pub fn from_hex(text: &str) -> Result<Self, crate::CryptoError> {
        Ok(Self(crate::hex::decode(text)?))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_are_redacted() {
        let secret = Secret::new(b"hunter2".to_vec());
        assert_eq!(format!("{secret:?}"), "[redacted]");
        assert_eq!(format!("{secret}"), "[redacted]");
        assert_eq!(secret.expose(), b"hunter2");
        assert_eq!(secret.to_hex(), "68756e74657232");
    }
}
