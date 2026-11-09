//! Provision, rotate, and deliver the per-VM data key over the control socket.

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use ternvale_config::VmConfig;
use ternvale_crypto::{
    unwrap_file_in_place, verify_dek, wrap_file_in_place, CryptoContext, Secret, AAD_TPM,
};
use ternvale_vmm::VmControl;

use crate::keychain;
use crate::paths;
use crate::protocol::{DataKeyHex, Request};
use crate::secret::Secret as CliSecret;

/// Install `key_hex` on `control`. Wrong key → authenticated error, no disk writes.
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all, fields(vm = control.name()))]
pub fn apply_data_key(
    control: &VmControl,
    key_hex: &str,
) -> Result<(), ternvale_crypto::CryptoError> {
    let dek = Secret::from_hex(key_hex)?;
    if let Ok(path) = paths::dek_path(control.name()) {
        if path.is_file() {
            let blob = std::fs::read(&path).map_err(|source| ternvale_crypto::CryptoError::Io {
                path: path.clone(),
                source,
            })?;
            verify_dek(&dek, &blob)?;
        }
    }
    let ctx = CryptoContext::from_dek(dek)?;
    control.set_crypto(Arc::new(ctx));
    Ok(())
}

/// Ensure a wrapping key and wrapped DEK exist for `vm`.
#[tracing::instrument(level = "info", target = "ternvale::crypto", skip_all, fields(vm))]
pub fn provision(vm: &str) -> Result<Secret> {
    let path = paths::dek_path(vm)?;
    let account = keychain::wrapping_key_account(vm);
    if path.is_file() {
        return unwrap_local(vm);
    }
    let wrapping = Secret::random_32().context("generate wrapping key")?;
    let dek = Secret::random_32().context("generate data key")?;
    keychain::store(&account, &CliSecret::new(wrapping.to_hex()))
        .with_context(|| format!("store wrapping key for {vm} in the Keychain"))?;
    ternvale_crypto::store_dek(&wrapping, &dek, &path)
        .with_context(|| format!("write wrapped DEK {}", path.display()))?;
    tracing::info!(
        target: "ternvale::crypto",
        vm,
        path = %path.display(),
        "provisioned wrapping key (Keychain) and wrapped data key"
    );
    Ok(dek)
}

/// Unwrap the DEK using the Keychain wrapping key.
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all, fields(vm))]
pub fn unwrap_local(vm: &str) -> Result<Secret> {
    let path = paths::dek_path(vm)?;
    let wrapping = load_wrapping(vm)?;
    ternvale_crypto::load_dek(&wrapping, &path)
        .with_context(|| format!("unwrap data key {}", path.display()))
}

/// Re-wrap the DEK with a new wrapping key. Disk/NVRAM/snapshot ciphertext is unchanged.
#[tracing::instrument(level = "info", target = "ternvale::crypto", skip_all, fields(vm))]
pub fn rotate(vm: &str) -> Result<()> {
    let path = paths::dek_path(vm)?;
    if !path.is_file() {
        bail!(
            "no wrapped data key at {} (enable encryption = true first)",
            path.display()
        );
    }
    let old = load_wrapping(vm)?;
    let blob = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let new = Secret::random_32().context("generate new wrapping key")?;
    let rotated = ternvale_crypto::rewrap(&old, &new, &blob).context("re-wrap data key")?;
    std::fs::write(&path, rotated).with_context(|| format!("write {}", path.display()))?;
    keychain::store(
        &keychain::wrapping_key_account(vm),
        &CliSecret::new(new.to_hex()),
    )
    .context("store rotated wrapping key")?;
    println!("rotated wrapping key for {vm} (data not re-encrypted)");
    tracing::info!(target: "ternvale::crypto", vm, "wrapping key rotated");
    Ok(())
}

/// Copy the source DEK to `dest`, wrapped with a new wrapping key (linked clones share ciphertext).
#[tracing::instrument(
    level = "info",
    target = "ternvale::crypto",
    skip_all,
    fields(source, dest)
)]
pub fn clone_keys(source: &str, dest: &str) -> Result<()> {
    let dek =
        unwrap_local(source).with_context(|| format!("unwrap source VM {source} data key"))?;
    let wrapping = Secret::random_32().context("generate clone wrapping key")?;
    keychain::store(
        &keychain::wrapping_key_account(dest),
        &CliSecret::new(wrapping.to_hex()),
    )
    .with_context(|| format!("store wrapping key for clone {dest}"))?;
    let path = paths::dek_path(dest)?;
    ternvale_crypto::store_dek(&wrapping, &dek, &path)
        .with_context(|| format!("write clone DEK {}", path.display()))?;
    tracing::info!(
        target: "ternvale::crypto",
        source,
        dest,
        "cloned data key (re-wrapped; layers stay ciphertext)"
    );
    Ok(())
}

/// Unwrap (or provision) the DEK and send it on the control socket. Never argv/env.
#[tracing::instrument(level = "info", target = "ternvale::crypto", skip_all, fields(vm = %config.name))]
pub fn deliver_over_control(config: &VmConfig, socket: &Path) -> Result<()> {
    if !config.encryption {
        return Ok(());
    }
    let dek = if paths::dek_path(&config.name)?.is_file() {
        unwrap_local(&config.name)?
    } else {
        provision(&config.name)?
    };
    let request = Request::SetDataKey {
        key_hex: DataKeyHex(dek.to_hex()),
    };
    let response = crate::client::request(socket, &config.name, &request)
        .context("send set-data-key over the control socket")?;
    if !response.ok {
        bail!(
            "set-data-key failed: {}",
            response.error.unwrap_or_else(|| "unknown error".into())
        );
    }
    tracing::info!(
        target: "ternvale::crypto",
        vm = %config.name,
        "data key delivered over the control socket"
    );
    Ok(())
}

/// Decrypt a GCM TPM state file so swtpm can use it.
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all, fields(path = %path.display()))]
pub fn unwrap_tpm_state(vm: &str, path: &Path) -> Result<()> {
    let dek = unwrap_local(vm)?;
    let ctx = CryptoContext::from_dek(dek).context("derive GCM key")?;
    unwrap_file_in_place(ctx.gcm(), path, AAD_TPM).context("unwrap TPM state")?;
    Ok(())
}

/// Encrypt TPM state after swtpm exits.
#[tracing::instrument(level = "debug", target = "ternvale::crypto", skip_all, fields(path = %path.display()))]
pub fn wrap_tpm_state(vm: &str, path: &Path) -> Result<()> {
    let dek = unwrap_local(vm)?;
    let ctx = CryptoContext::from_dek(dek).context("derive GCM key")?;
    wrap_file_in_place(ctx.gcm(), path, AAD_TPM).context("wrap TPM state")?;
    Ok(())
}

fn load_wrapping(vm: &str) -> Result<Secret> {
    let account = keychain::wrapping_key_account(vm);
    let Some(found) = keychain::find(&account).context("read wrapping key from Keychain")? else {
        bail!("no wrapping key in the Keychain for {vm} ({account})");
    };
    Secret::from_hex(found.expose()).context("parse wrapping key hex")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternvale_crypto::wrap_dek;

    #[test]
    fn apply_rejects_wrong_key() {
        let dir = std::env::temp_dir().join(format!("tv-crypto-cmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        // SAFETY: this test owns TERNVALE_VMS_DIR; run with --test-threads=1.
        unsafe {
            std::env::set_var(crate::paths::VMS_DIR_ENV, &dir);
        }
        let wrap = Secret::new(vec![1u8; 32]);
        let dek = Secret::new(vec![2u8; 32]);
        let blob = wrap_dek(&wrap, &dek).expect("wrap");
        let vm_dir = dir.join("t");
        std::fs::create_dir_all(&vm_dir).expect("vm");
        std::fs::write(vm_dir.join("dek.tkey"), blob).expect("dek");
        let before = std::fs::read(vm_dir.join("dek.tkey")).expect("before");
        let control = VmControl::new("t", 1);
        apply_data_key(&control, &dek.to_hex()).expect("ok");
        assert!(control.crypto().is_some());
        let wrong = Secret::new(vec![9u8; 32]);
        let err = apply_data_key(&control, &wrong.to_hex()).expect_err("wrong");
        assert!(err.is_auth(), "{err}");
        let after = std::fs::read(vm_dir.join("dek.tkey")).expect("after");
        assert_eq!(before, after, "wrong key must not rewrite dek.tkey");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
