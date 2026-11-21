//! Embedded **public** verifying key. The matching private key is never in
//! this repository.
//!
//! Replace [`EMBEDDED_PUBLIC_HEX`] with the real 32-byte verifying key before
//! shipping. Tests inject a SigningKey and do not use this constant.

/// 32-byte Ed25519 verifying key, hex. Replace with the production verifying
/// key before shipping. This placeholder is the public half of a discarded
/// development seed (the seed is **not** in this repository).
pub const EMBEDDED_PUBLIC_HEX: &str =
    "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c";

/// Parse [`EMBEDDED_PUBLIC_HEX`].
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn embedded_verifying_key() -> Result<ed25519_dalek::VerifyingKey, crate::LicenseError> {
    let mut bytes = [0u8; 32];
    hex_decode(EMBEDDED_PUBLIC_HEX, &mut bytes)?;
    ed25519_dalek::VerifyingKey::from_bytes(&bytes)
        .map_err(|_| crate::LicenseError::Verify("embedded public key is invalid".into()))
}

#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
fn hex_decode(s: &str, out: &mut [u8]) -> Result<(), crate::LicenseError> {
    if s.len() != out.len() * 2 {
        return Err(crate::LicenseError::Verify("public key hex length".into()));
    }
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|_| crate::LicenseError::Verify("public key hex".into()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_placeholder_is_a_valid_point() {
        embedded_verifying_key().expect("replace EMBEDDED_PUBLIC_HEX with a valid verifying key");
    }
}
