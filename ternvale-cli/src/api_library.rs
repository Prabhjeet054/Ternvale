//! Library operations used by the JSON-RPC hub (`list` / `create` / …).

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use ternvale_backend::{resolve, BackendCapabilities};
use ternvale_config::{BackendKind, IsoSection, NicMode, UsbPassthroughEntry, VmConfig};

use crate::client::{self, Reach};
use crate::paths;
use crate::protocol::{Request, Response, VmListEntry};

/// Scan `vms_root` for configs.
#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
pub fn list_vms() -> Result<Vec<VmListEntry>> {
    let root = paths::vms_root()?;
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut vms = Vec::new();
    for entry in fs::read_dir(&root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry.with_context(|| format!("read entry in {}", root.display()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let config = paths::vm_config_path(name)?;
        if !config.is_file() {
            continue;
        }
        let running = vm_is_running(name)?;
        let (backend, capabilities) = match fs::read_to_string(&config) {
            Ok(text) => match VmConfig::parse(&text) {
                Ok(cfg) => {
                    let selection = resolve(&cfg);
                    let caps = (selection.resolved == BackendKind::Native)
                        .then(BackendCapabilities::native);
                    (Some(selection.resolved.to_string()), caps)
                }
                Err(error) => {
                    tracing::debug!(
                        target: "ternvale::api",
                        vm = name,
                        error = %error,
                        "list: could not parse config for backend fields"
                    );
                    (None, None)
                }
            },
            Err(error) => {
                tracing::debug!(
                    target: "ternvale::api",
                    vm = name,
                    error = %error,
                    "list: could not read config for backend fields"
                );
                (None, None)
            }
        };
        vms.push(VmListEntry {
            name: name.to_string(),
            running,
            state: None,
            backend,
            capabilities,
        });
    }
    vms.sort_by(|a, b| a.name.cmp(&b.name));
    tracing::info!(target: "ternvale::api", count = vms.len(), "listed vms");
    Ok(vms)
}

#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm = name))]
pub fn vm_is_running(name: &str) -> Result<bool> {
    let path = paths::socket_path(name)?;
    match client::connect(&path)? {
        Ok(_) => Ok(true),
        Err(Reach::NotRunning | Reach::Stale) => Ok(false),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm = name))]
pub fn create_vm(name: &str, config_toml: Option<&str>, config_path: Option<&Path>) -> Result<()> {
    paths::socket_path(name)?;
    let dest = paths::vm_config_path(name)?;
    if dest.exists() {
        bail!("VM {name:?} already exists at {}", dest.display());
    }
    let text = if let Some(path) = config_path {
        fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
    } else if let Some(toml) = config_toml {
        toml.to_string()
    } else {
        bail!("create requires config_toml or config_path");
    };
    let config = VmConfig::from_toml(&text).context("parse VM config")?;
    if config.name != name {
        bail!(
            "config name {:?} does not match create name {name:?}",
            config.name
        );
    }
    fs::create_dir_all(paths::vm_dir(name)?).context("create VM directory")?;
    fs::write(&dest, config.to_toml().context("serialize config")?)
        .with_context(|| format!("write {}", dest.display()))?;
    tracing::info!(target: "ternvale::api", vm = name, path = %dest.display(), "created vm");
    Ok(())
}

#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm = name))]
pub fn delete_stopped_vm(name: &str) -> Result<()> {
    if vm_is_running(name)? {
        bail!("cannot delete running VM {name}");
    }
    crate::clone_cmd::delete_vm(name)?;
    Ok(())
}

#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm = name))]
pub fn start_vm(name: &str) -> Result<()> {
    if vm_is_running(name)? {
        bail!("VM {name} is already running");
    }
    let config = paths::vm_config_path(name)?;
    if !config.is_file() {
        bail!("no VM {name:?} at {}", config.display());
    }
    let bin = std::env::var_os("TERNVALE_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_exe().context("current_exe for ternvale run")?);
    Command::new(&bin)
        .arg("run")
        .arg(&config)
        .spawn()
        .with_context(|| format!("spawn {} run {}", bin.display(), config.display()))?;
    tracing::info!(target: "ternvale::api", vm = name, "started vm process");
    Ok(())
}

#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm = name, cmd = request.name()))]
pub fn forward(name: &str, request: &Request) -> Result<Response> {
    let path = paths::socket_path(name)?;
    client::request(&path, name, request)
}

fn load_stopped(name: &str) -> Result<(VmConfig, std::path::PathBuf)> {
    if vm_is_running(name)? {
        bail!("VM {name} is running; stop it before changing media, USB, or network config");
    }
    let path = paths::vm_config_path(name)?;
    let config = VmConfig::from_path(&path).with_context(|| format!("load {}", path.display()))?;
    Ok((config, path))
}

fn save(config: &VmConfig, path: &Path) -> Result<()> {
    config.validate().context("validate VM config")?;
    fs::write(path, config.to_toml().context("serialize config")?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm = name, slot))]
pub fn media_attach(name: &str, slot: &str, image: &Path) -> Result<()> {
    let (mut config, path) = load_stopped(name)?;
    let mut iso = config.iso.clone().unwrap_or_default();
    set_slot(&mut iso, slot, Some(image.to_path_buf()))?;
    config.iso = Some(iso);
    save(&config, &path)?;
    tracing::info!(target: "ternvale::api", vm = name, slot, path = %image.display(), "media attached");
    Ok(())
}

#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm = name, slot))]
pub fn media_detach(name: &str, slot: &str) -> Result<()> {
    let (mut config, path) = load_stopped(name)?;
    let mut iso = config.iso.clone().unwrap_or_default();
    set_slot(&mut iso, slot, None)?;
    config.iso = Some(iso);
    save(&config, &path)?;
    tracing::info!(target: "ternvale::api", vm = name, slot, "media detached");
    Ok(())
}

fn set_slot(iso: &mut IsoSection, slot: &str, path: Option<std::path::PathBuf>) -> Result<()> {
    match slot {
        "installer" => iso.installer = path,
        "tools" => iso.tools = path,
        "drivers" => iso.drivers = path,
        "unattend" => iso.unattend = path,
        other => bail!("unknown media slot {other:?}"),
    }
    Ok(())
}

#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm = name))]
pub fn usb_attach(name: &str, vendor_id: u16, product_id: u16, port: Option<u8>) -> Result<()> {
    let (mut config, path) = load_stopped(name)?;
    let mut usb = config.usb.clone().unwrap_or_default();
    usb.passthrough.push(UsbPassthroughEntry {
        vendor_id,
        product_id,
        port,
    });
    config.usb = Some(usb);
    save(&config, &path)?;
    tracing::info!(
        target: "ternvale::api",
        vm = name,
        vendor_id = format!("{:#06x}", vendor_id),
        product_id = format!("{:#06x}", product_id),
        "usb attach recorded"
    );
    Ok(())
}

#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm = name, index))]
pub fn net_configure(
    name: &str,
    index: usize,
    backend: Option<String>,
    mode: Option<String>,
    network: Option<String>,
    bridge_interface: Option<String>,
) -> Result<()> {
    let (mut config, path) = load_stopped(name)?;
    let nic = config
        .nics
        .get_mut(index)
        .with_context(|| format!("no NIC at index {index}"))?;
    if let Some(backend) = backend {
        nic.backend = backend;
    }
    if let Some(mode) = mode {
        nic.mode = match mode.as_str() {
            "shared" => NicMode::Shared,
            "host-only" => NicMode::HostOnly,
            "bridged" => NicMode::Bridged,
            other => bail!("unknown nic mode {other:?}"),
        };
    }
    if network.is_some() {
        nic.network = network;
    }
    if bridge_interface.is_some() {
        nic.bridge_interface = bridge_interface;
    }
    save(&config, &path)?;
    tracing::info!(target: "ternvale::api", vm = name, index, "net configured");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::set_slot;
    use ternvale_config::IsoSection;

    #[test]
    fn rejects_unknown_media_slot() {
        let mut iso = IsoSection::default();
        assert!(set_slot(&mut iso, "dvd", None).is_err());
        set_slot(&mut iso, "tools", None).expect("tools");
    }
}
