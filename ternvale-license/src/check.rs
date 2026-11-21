//! Resolve trial vs license vs degraded.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::VerifyingKey;

use crate::blob::{self, LicenseBlob};
use crate::status::{self, Entitlement};
use crate::trial::{self, KeychainStore, TrialStore};
use crate::{pubkey, LicenseError};

/// Env: path to a `TVLK` file.
pub const LICENSE_FILE_ENV: &str = "TERNVALE_LICENSE_FILE";
/// Env: optional `http://host/path` activation URL.
pub const ACTIVATE_URL_ENV: &str = "TERNVALE_LICENSE_ACTIVATE_URL";

/// Inputs for [`evaluate`]. Tests inject key, clock, and store.
pub struct EvaluateInput<'a> {
    /// Verifying key (production: [`pubkey::embedded_verifying_key`]).
    pub public: &'a VerifyingKey,
    /// Trial / last-seen store.
    pub store: &'a dyn TrialStore,
    /// Optional on-disk license bytes.
    pub license_bytes: Option<&'a [u8]>,
    /// Wall clock unix seconds.
    pub now: u64,
    /// If set, POST `license_id` only (never VM data).
    pub activate_url: Option<&'a str>,
}

/// Default license path (overridden by [`LICENSE_FILE_ENV`]).
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn default_license_path() -> PathBuf {
    if let Some(p) = std::env::var_os(LICENSE_FILE_ENV) {
        return PathBuf::from(p);
    }
    trial::default_file_store().path().join("license.tvlk")
}

/// Unix now; 0 if the clock is before the epoch.
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Verify license or start/continue trial.
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn evaluate(input: EvaluateInput<'_>) -> Result<Entitlement, LicenseError> {
    let now = trial::eval_now(input.store, input.now)?;
    if let Some(bytes) = input.license_bytes {
        let blob = blob::verify(bytes, input.public)?;
        if let Some(url) = input.activate_url {
            if let Err(error) = crate::activate::activate(url, &blob.payload.id) {
                tracing::warn!(
                    target: "ternvale::license",
                    error = %error,
                    "optional activation failed; continuing offline"
                );
            }
        }
        return Ok(status::from_license(blob.payload, now));
    }
    let start = trial::ensure_start(input.store, now)?;
    Ok(status::from_trial(start, now))
}

/// Production check: Keychain trial, embedded public key, optional file.
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn current() -> Result<Entitlement, LicenseError> {
    let store = KeychainStore::production();
    let public = pubkey::embedded_verifying_key()?;
    let path = default_license_path();
    let bytes = read_license_file(&path)?;
    let url = std::env::var(ACTIVATE_URL_ENV).ok();
    evaluate(EvaluateInput {
        public: &public,
        store: &store,
        license_bytes: bytes.as_deref(),
        now: unix_now(),
        activate_url: url.as_deref(),
    })
}

#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
fn read_license_file(path: &Path) -> Result<Option<Vec<u8>>, LicenseError> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(LicenseError::io("read license file", e)),
    }
}

/// Copy a verified license into the default path.
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn install_file(src: &Path, public: &VerifyingKey) -> Result<LicenseBlob, LicenseError> {
    let bytes = std::fs::read(src).map_err(|e| LicenseError::io("read license to install", e))?;
    let blob = blob::verify(&bytes, public)?;
    let dest = default_license_path();
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| LicenseError::io("mkdir license dir", e))?;
    }
    std::fs::write(&dest, &bytes).map_err(|e| LicenseError::io("write license file", e))?;
    tracing::info!(
        target: "ternvale::license",
        id = %blob.payload.id,
        path = %dest.display(),
        "license installed"
    );
    Ok(blob)
}

/// Refuse create/start when degraded.
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn require_new_session(entitlement: &Entitlement) -> Result<(), LicenseError> {
    if entitlement.allows_new_session() {
        return Ok(());
    }
    let reason = match entitlement {
        Entitlement::Degraded { reason } => reason.clone(),
        _ => "license does not allow new VM sessions".into(),
    };
    Err(LicenseError::Degraded(reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::sign;
    use crate::payload::{LicensePayload, PRODUCT};
    use crate::status::{GRACE_DAYS, TRIAL_DAYS};
    use crate::trial::MemoryStore;
    use ed25519_dalek::SigningKey;

    fn sk() -> SigningKey {
        SigningKey::from_bytes(&[9u8; 32])
    }

    fn payload(expiry: u64) -> LicensePayload {
        LicensePayload {
            product: PRODUCT.into(),
            edition: "team".into(),
            seats: 3,
            expiry_unix: expiry,
            issued_unix: 1,
            id: "lic-1".into(),
        }
    }

    #[test]
    fn trial_then_degraded() {
        let store = MemoryStore::default();
        let sk = sk();
        let vk = sk.verifying_key();
        let e = evaluate(EvaluateInput {
            public: &vk,
            store: &store,
            license_bytes: None,
            now: 100,
            activate_url: None,
        })
        .expect("trial");
        assert!(matches!(e, Entitlement::Trial { .. }));
        assert!(e.allows_new_session());
        let late = 100 + (TRIAL_DAYS + GRACE_DAYS) * 86_400;
        let e = evaluate(EvaluateInput {
            public: &vk,
            store: &store,
            license_bytes: None,
            now: late,
            activate_url: None,
        })
        .expect("expired trial");
        assert!(!e.allows_new_session());
        assert!(e.allows_open_export_backup());
        assert!(require_new_session(&e).is_err());
    }

    #[test]
    fn expired_license_is_degraded_after_grace() {
        let store = MemoryStore::default();
        let sk = sk();
        let vk = sk.verifying_key();
        let blob = sign(&payload(1_000), &sk).expect("sign");
        let late = 1_000 + GRACE_DAYS * 86_400;
        let e = evaluate(EvaluateInput {
            public: &vk,
            store: &store,
            license_bytes: Some(&blob),
            now: late,
            activate_url: None,
        })
        .expect("expired");
        assert!(matches!(e, Entitlement::Degraded { .. }));
        let err = require_new_session(&e).expect_err("refuse start");
        assert!(
            err.to_string().contains("open, export, and backup"),
            "{err}"
        );
        assert!(e.allows_open_export_backup());
    }

    #[test]
    fn licensed_file_overrides_trial() {
        let store = MemoryStore::default();
        let sk = sk();
        let vk = sk.verifying_key();
        let blob = sign(&payload(50_000), &sk).expect("sign");
        let e = evaluate(EvaluateInput {
            public: &vk,
            store: &store,
            license_bytes: Some(&blob),
            now: 1_000,
            activate_url: None,
        })
        .expect("lic");
        assert!(matches!(e, Entitlement::Licensed { .. }));
    }

    #[test]
    fn wrong_edition_rejected() {
        let sk = sk();
        let mut p = payload(50_000);
        p.edition = "enterprise".into();
        let json = serde_json::to_vec(&p).expect("json");
        let sig = {
            use ed25519_dalek::Signer;
            sk.sign(&json)
        };
        let mut blob = Vec::from(*crate::blob::MAGIC);
        blob.extend_from_slice(&crate::blob::VERSION.to_le_bytes());
        blob.extend_from_slice(&json);
        blob.extend_from_slice(&sig.to_bytes());
        let err = blob::verify(&blob, &sk.verifying_key()).expect_err("edition");
        assert!(err.to_string().contains("wrong edition"));
    }
}
