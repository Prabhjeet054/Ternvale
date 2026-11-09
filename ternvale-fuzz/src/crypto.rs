//! Fuzz wrapped-DEK (`TVKW`) and GCM envelope (`TVGE`) parsers.

use ternvale_crypto::{unwrap_dek, GcmKey, Secret};

/// Hostile wrap-file and GCM-envelope bytes must not panic.
#[tracing::instrument(level = "trace", target = "ternvale::fuzz", skip_all)]
pub fn run(data: &[u8]) {
    crate::quiet();
    let wrapping = Secret::new(vec![0x11; 32]);
    let _ = unwrap_dek(&wrapping, data);
    if let Ok(gcm) = GcmKey::from_key(&Secret::new(vec![0x22; 32])) {
        let _ = gcm.open(data, b"fuzz");
        let _ = gcm.open(data, b"nvram");
    }
}
