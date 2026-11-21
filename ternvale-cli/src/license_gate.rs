//! CLI/API gate: new VM sessions need a trial, license, or grace.

use anyhow::{bail, Result};

/// When set to `degraded` under `cfg!(test)`, new sessions are refused so
/// quality tests can exercise the expired-state message without Keychain.
pub const TEST_FORCE_ENV: &str = "TERNVALE_LICENSE_TEST_FORCE";

/// Skip the Keychain trial during `cargo test -p ternvale-cli` (no real VM).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn require_new_session() -> Result<()> {
    if cfg!(test) {
        if std::env::var_os(TEST_FORCE_ENV).as_deref() == Some(std::ffi::OsStr::new("degraded")) {
            tracing::warn!(
                target: "ternvale::cli",
                "test force: degraded (open/export/backup only)"
            );
            bail!("license expired; open, export, and backup remain available");
        }
        return Ok(());
    }
    let entitlement = ternvale_license::current()?;
    match ternvale_license::require_new_session(&entitlement) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!(target: "ternvale::cli", error = %e, "new VM session refused");
            bail!("{e}")
        }
    }
}

/// Install a `TVLK` file (public-key verify only).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn install(path: &std::path::Path) -> Result<()> {
    let public = ternvale_license::pubkey::embedded_verifying_key()?;
    let blob = ternvale_license::install_file(path, &public)?;
    println!(
        "installed license {} ({}, {} seat(s))",
        blob.payload.id, blob.payload.edition, blob.payload.seats
    );
    Ok(())
}

/// Print current entitlement (no secrets).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn show() -> Result<()> {
    let e = ternvale_license::current()?;
    match e {
        ternvale_license::Entitlement::Licensed { payload } => {
            println!(
                "licensed id={} edition={} seats={} expiry_unix={}",
                payload.id, payload.edition, payload.seats, payload.expiry_unix
            );
        }
        ternvale_license::Entitlement::Trial { ends_unix, .. } => {
            println!("trial active until unix {ends_unix}");
        }
        ternvale_license::Entitlement::Grace { ends_unix, .. } => {
            println!("grace period until unix {ends_unix} (open/export/backup still allowed)");
        }
        ternvale_license::Entitlement::Degraded { reason } => {
            println!("{reason}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ternvale_license::status::Entitlement;

    #[test]
    fn export_allowed_when_degraded() {
        let e = Entitlement::Degraded {
            reason: "trial ended".into(),
        };
        assert!(e.allows_open_export_backup());
        assert!(!e.allows_new_session());
    }
}
