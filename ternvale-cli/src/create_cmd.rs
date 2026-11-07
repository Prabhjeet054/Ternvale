//! `ternvale create --windows --iso --unattended`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use ternvale_config::{disc_format, VmConfig, WINDOWS_MIN_DISK_BYTES};

use crate::disk;
use crate::keychain;
use crate::paths;
use crate::secret::Secret;
use crate::tools;
use crate::unattend_iso;
use crate::unattend_xml::UnattendSpec;

/// Default local account when `--user` is omitted.
pub const DEFAULT_USER: &str = "Ternvale";
/// Default `/IMAGE/NAME`.
pub const DEFAULT_EDITION: &str = "Windows 11 Pro";
/// Default system disk.
pub const DEFAULT_DISK: &str = "64G";

/// Flags after clap parse (no secret values).
pub struct CreateWindows {
    /// Library VM name.
    pub name: String,
    /// User Windows 11 ARM64 ISO.
    pub iso: PathBuf,
    /// Generate Autounattend.xml and attach it.
    pub unattended: bool,
    /// `/IMAGE/NAME`.
    pub edition: String,
    /// Disk size text (`64G`).
    pub disk_size: String,
    /// UEFI firmware image.
    pub firmware: Option<PathBuf>,
    /// Tools ISO override.
    pub tools_iso: Option<PathBuf>,
    /// Local account.
    pub user: String,
    /// Env var holding the password.
    pub password_env: Option<String>,
    /// File holding the password (one line).
    pub password_file: Option<PathBuf>,
    /// Env var holding the product key.
    pub product_key_env: Option<String>,
    /// File holding the product key.
    pub product_key_file: Option<PathBuf>,
    /// Poll until the guest agent connects.
    pub wait: bool,
    /// `--wait` timeout.
    pub timeout_secs: u64,
}

/// `ternvale create --windows …`.
#[tracing::instrument(level = "info", target = "ternvale::cli", skip_all, fields(vm = %args.name, unattended = args.unattended))]
pub fn create_windows(args: CreateWindows) -> Result<ExitCode> {
    paths::socket_path(&args.name)?;
    if !args.unattended {
        bail!("`ternvale create --windows` requires --unattended (interactive Setup is not wired)");
    }
    let installer = args.iso.canonicalize().with_context(|| {
        format!(
            "Windows ISO {} (you supply this image and licence; Ternvale does not bundle keys)",
            args.iso.display()
        )
    })?;
    match disc_format(&installer) {
        Ok(Some(_)) => {}
        Ok(None) => bail!("--iso {} is not an ISO 9660/UDF disc", installer.display()),
        Err(error) => {
            return Err(error).with_context(|| format!("read Windows ISO {}", installer.display()))
        }
    }
    let tools_iso = args
        .tools_iso
        .clone()
        .map(Ok)
        .unwrap_or_else(tools::default_tools_iso)?;
    unattend_iso::require_tools_iso(&tools_iso)?;
    let firmware = resolve_firmware(args.firmware.as_deref())?;
    let password = load_secret(
        "password",
        args.password_env.as_deref(),
        args.password_file.as_deref(),
        Some(&keychain::password_account(&args.name)),
        true,
    )?;
    let product_key = load_secret(
        "product key",
        args.product_key_env.as_deref(),
        args.product_key_file.as_deref(),
        Some(&keychain::product_key_account(&args.name)),
        false,
    )?;
    let dest = paths::vm_dir(&args.name)?;
    if dest.exists() {
        bail!("VM {:?} already exists at {}", args.name, dest.display());
    }
    fs::create_dir_all(dest.join("disks")).context("create VM disks directory")?;
    let disk_path = dest.join("disks/system.img");
    let disk_bytes = disk::parse_size(&args.disk_size)?;
    if disk_bytes < WINDOWS_MIN_DISK_BYTES {
        tracing::warn!(
            target: "ternvale::cli",
            bytes = disk_bytes,
            "disk is smaller than Windows 11's 64 GiB minimum"
        );
    }
    disk::create_disk(&disk_path, disk_bytes)?;
    let unattend_path = dest.join("unattend.iso");
    let spec = UnattendSpec {
        user: &args.user,
        password: &password,
        product_key: if product_key.is_empty() {
            None
        } else {
            Some(&product_key)
        },
        edition: &args.edition,
        computer_name: &args.name,
    };
    unattend_iso::build_unattend_iso(&unattend_path, &tools_iso, &spec)?;
    if std::env::var_os("TERNVALE_SKIP_KEYCHAIN").is_none() {
        keychain::store(&keychain::password_account(&args.name), &password)?;
        if !product_key.is_empty() {
            keychain::store(&keychain::product_key_account(&args.name), &product_key)?;
        }
    } else {
        tracing::debug!(target: "ternvale::cli", "TERNVALE_SKIP_KEYCHAIN set; not storing secrets");
    }
    let config = windows_config(
        &args.name,
        &dest,
        &disk_path,
        &firmware,
        &installer,
        &tools_iso,
        &unattend_path,
    )?;
    config
        .validate()
        .context("validate generated Windows config")?;
    let config_path = paths::vm_config_path(&args.name)?;
    let text = config.to_toml().context("serialize VM config")?;
    assert_no_secret(&text, password.expose(), "config.toml")?;
    if !product_key.is_empty() {
        assert_no_secret(&text, product_key.expose(), "config.toml")?;
    }
    fs::write(&config_path, text).with_context(|| format!("write {}", config_path.display()))?;
    tracing::info!(
        target: "ternvale::cli",
        vm = %args.name,
        config = %config_path.display(),
        "created unattended Windows VM"
    );
    println!("created {}", args.name);
    println!("  config: {}", config_path.display());
    println!("  disk: {}", disk_path.display());
    println!("  installer: {}", installer.display());
    println!("  tools: {}", tools_iso.display());
    println!("  unattend: {}", unattend_path.display());
    println!("  password: stored in Keychain (never logged)");
    if args.wait {
        return crate::create_progress::wait_for_agent(
            &config_path,
            dest.join("serial.log").as_path(),
            &args.name,
            args.timeout_secs,
        );
    }
    println!("start with: ternvale run {}", config_path.display());
    Ok(ExitCode::SUCCESS)
}

fn windows_config(
    name: &str,
    dest: &Path,
    disk: &Path,
    firmware: &Path,
    installer: &Path,
    tools: &Path,
    unattend: &Path,
) -> Result<VmConfig> {
    let config = VmConfig::parse(&format!(
        "name = {name}\nprofile = \"windows\"\ncpus = 2\nram_mib = 4096\nfirmware = {fw}\nserial_log = {serial}\n\n[[disks]]\npath = {disk}\nread_only = false\n\n[iso]\ninstaller = {installer}\ntools = {tools}\nunattend = {unattend}\n\n[[nics]]\nbackend = \"user\"\n\n[vsock]\nagent = true\nserial = true\n",
        name = toml_string(name),
        fw = toml_string(&firmware.display().to_string()),
        serial = toml_string(&dest.join("serial.log").display().to_string()),
        disk = toml_string(&disk.display().to_string()),
        installer = toml_string(&installer.display().to_string()),
        tools = toml_string(&tools.display().to_string()),
        unattend = toml_string(&unattend.display().to_string()),
    ))
    .context("parse generated Windows config")?;
    Ok(config)
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
fn resolve_firmware(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        bail!("firmware is not a file: {}", path.display());
    }
    if let Some(path) = std::env::var_os("TERNVALE_FIRMWARE") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!("TERNVALE_FIRMWARE is not a file: {}", path.display());
    }
    let repo =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-assets/firmware/QEMU_EFI.fd");
    if repo.is_file() {
        return Ok(repo);
    }
    bail!("need --firmware PATH or TERNVALE_FIRMWARE (scripts/fetch-firmware.sh)")
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(kind))]
fn load_secret(
    kind: &str,
    env_name: Option<&str>,
    file: Option<&Path>,
    keychain_account: Option<&str>,
    required: bool,
) -> Result<Secret> {
    if let Some(name) = env_name {
        let value = std::env::var(name).with_context(|| format!("{kind} env {name} is unset"))?;
        if value.is_empty() && required {
            bail!("{kind} env {name} is empty");
        }
        return Ok(Secret::new(value));
    }
    if let Some(path) = file {
        let mut text = fs::read_to_string(path)
            .with_context(|| format!("read {kind} file {}", path.display()))?;
        while text.ends_with(['\n', '\r']) {
            text.pop();
        }
        if text.is_empty() && required {
            bail!("{kind} file {} is empty", path.display());
        }
        return Ok(Secret::new(text));
    }
    if let Some(account) = keychain_account {
        if let Some(found) = keychain::find(account)? {
            if !found.is_empty() || !required {
                return Ok(found);
            }
        }
    }
    if required {
        bail!(
            "{kind} required via --password-env/--password-file (or --product-key-env/--product-key-file); do not put secrets on the command line"
        );
    }
    Ok(Secret::new(String::new()))
}

fn assert_no_secret(text: &str, secret: &str, where_: &str) -> Result<()> {
    if !secret.is_empty() && text.contains(secret) {
        tracing::error!(target: "ternvale::cli", where_, "refusing to write a secret into {where_}");
        bail!("{where_} would have contained a secret; aborting");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_toml_has_no_password() {
        let text = toml_string("hello");
        assert_eq!(text, "\"hello\"");
        assert!(assert_no_secret("name = \"x\"", "secret", "config").is_ok());
        assert!(assert_no_secret("pw = secret", "secret", "config").is_err());
    }
}
