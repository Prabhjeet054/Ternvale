//! macOS Keychain for Windows passwords and product keys.
//!
//! Uses `/usr/bin/security` so the VMM process does not grow extra entitlements.
//! Secret bytes are never written to tracing fields.

use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::secret::Secret;

/// Keychain service id.
pub const SERVICE: &str = "com.ternvale.vm";

/// Account for the local Windows password.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm))]
pub fn password_account(vm: &str) -> String {
    format!("{vm}/windows-password")
}

/// Account for the optional product key.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm))]
pub fn product_key_account(vm: &str) -> String {
    format!("{vm}/windows-product-key")
}

/// Account for the 32-byte wrapping key (hex) that protects the per-VM DEK.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm))]
pub fn wrapping_key_account(vm: &str) -> String {
    format!("{vm}/wrapping-key")
}

/// Store `secret` under `account`.
#[tracing::instrument(level = "info", target = "ternvale::cli", skip_all, fields(account))]
pub fn store(account: &str, secret: &Secret) -> Result<()> {
    let output = Command::new("/usr/bin/security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            SERVICE,
            "-a",
            account,
            "-w",
            secret.expose(),
        ])
        .output()
        .context("spawn /usr/bin/security add-generic-password")?;
    if !output.status.success() {
        tracing::error!(
            target: "ternvale::cli",
            account,
            status = %output.status,
            "keychain store failed"
        );
        bail!(
            "could not store {account} in the Keychain ({})",
            output.status
        );
    }
    tracing::info!(target: "ternvale::cli", account, "stored secret in Keychain");
    Ok(())
}

/// Load a secret, if present.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(account))]
pub fn find(account: &str) -> Result<Option<Secret>> {
    let output = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", SERVICE, "-a", account, "-w"])
        .output()
        .context("spawn /usr/bin/security find-generic-password")?;
    if !output.status.success() {
        tracing::debug!(
            target: "ternvale::cli",
            account,
            status = %output.status,
            "no Keychain item"
        );
        return Ok(None);
    }
    let mut text = String::from_utf8(output.stdout).context("Keychain secret was not UTF-8")?;
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    Ok(Some(Secret::new(text)))
}
