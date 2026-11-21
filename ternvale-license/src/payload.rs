//! Signed license claims.

use serde::{Deserialize, Serialize};

/// Product id baked into every key.
pub const PRODUCT: &str = "ternvale";
/// Editions this build accepts.
pub const EDITIONS: &[&str] = &["personal", "pro", "team"];

/// Fields covered by the Ed25519 signature (canonical JSON object).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicensePayload {
    /// Must be [`PRODUCT`].
    pub product: String,
    /// `personal` / `pro` / `team`.
    pub edition: String,
    /// Concurrent seats (at least 1).
    pub seats: u32,
    /// Exclusive unix expiry (seconds).
    pub expiry_unix: u64,
    /// Issued-at unix seconds.
    pub issued_unix: u64,
    /// Opaque id (activation / support). Not a secret.
    pub id: String,
}

impl LicensePayload {
    /// Reject unknown product/edition and zero seats.
    #[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
    pub fn validate(&self) -> Result<(), crate::LicenseError> {
        if self.product != PRODUCT {
            return Err(crate::LicenseError::Verify(format!(
                "wrong product {:?}",
                self.product
            )));
        }
        if !EDITIONS.contains(&self.edition.as_str()) {
            return Err(crate::LicenseError::Verify(format!(
                "wrong edition {:?}",
                self.edition
            )));
        }
        if self.seats == 0 {
            return Err(crate::LicenseError::Verify(
                "seats must be at least 1".into(),
            ));
        }
        if self.id.is_empty() || self.id.len() > 128 {
            return Err(crate::LicenseError::Verify("invalid license id".into()));
        }
        Ok(())
    }
}
