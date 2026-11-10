//! `ternvale disk compact <vm>` and `ternvale disk resize <vm> <size>`.

use std::fs::{File, OpenOptions};
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use nix::unistd::{lseek, Whence};
use ternvale_config::VmConfig;
use ternvale_snapshot::{is_stack_manifest, ImageStack};

use crate::client;
use crate::disk::{self, human_size};
use crate::paths;
use crate::protocol::Request;

/// Rewrite delta layers (VM must be stopped).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(vm, disk, json)
)]
pub fn compact(vm: &str, disk: Option<u32>, json: bool) -> Result<ExitCode> {
    if vm_running(vm)? {
        bail!("disk compact requires the VM to be stopped");
    }
    let path = disk_path(vm, disk)?;
    let before = allocated_path(&path)?;
    let after = if is_stack_manifest(&path) {
        let mut stack = ImageStack::open(&path, false).context("open layered disk")?;
        let (_b, after) = stack.compact_layers().context("compact layered disk")?;
        after
    } else {
        tracing::info!(
            target: "ternvale::cli",
            path = %path.display(),
            before,
            after = before,
            "raw disk compact is a no-op"
        );
        before
    };
    tracing::info!(
        target: "ternvale::cli",
        vm,
        path = %path.display(),
        before,
        after,
        "disk compacted"
    );
    print_sizes(json, before, after, None)?;
    Ok(ExitCode::SUCCESS)
}

/// Grow a disk; if the VM is running, send `disk-resize` for a config interrupt.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(vm, size, force, disk, json)
)]
pub fn resize(
    vm: &str,
    size: &str,
    force: bool,
    disk: Option<u32>,
    json: bool,
) -> Result<ExitCode> {
    let bytes = disk::parse_size(size).with_context(|| format!("parse disk size {size:?}"))?;
    if vm_running(vm)? {
        return resize_running(vm, bytes, force, disk, json);
    }
    let path = disk_path(vm, disk)?;
    let before = allocated_path(&path)?;
    let in_use = in_use_path(&path)?;
    if bytes < in_use && !force {
        bail!(
            "refusing to shrink below in-use size {} (use --force)",
            human_size(in_use)
        );
    }
    if is_stack_manifest(&path) {
        let mut stack = ImageStack::open(&path, false).context("open layered disk")?;
        stack.resize(bytes).context("resize layered disk")?;
    } else {
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        file.set_len(bytes)
            .with_context(|| format!("set length of {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", path.display()))?;
    }
    let after = allocated_path(&path)?;
    tracing::info!(
        target: "ternvale::cli",
        vm,
        path = %path.display(),
        before,
        after,
        logical = bytes,
        "disk resized"
    );
    print_sizes(json, before, after, Some(bytes))?;
    Ok(ExitCode::SUCCESS)
}

fn resize_running(
    vm: &str,
    bytes: u64,
    force: bool,
    disk: Option<u32>,
    json: bool,
) -> Result<ExitCode> {
    let socket = paths::socket_path(vm)?;
    let stream = match client::connect(&socket)? {
        Ok(stream) => stream,
        Err(_) => bail!("vm {vm} is not running"),
    };
    let response = client::exchange(stream, &Request::DiskResize { bytes, force, disk })?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&response).context("encode disk-resize response")?
        );
    } else if !response.ok {
        bail!(
            "disk-resize failed: {}",
            response.error.as_deref().unwrap_or("unknown error")
        );
    } else {
        let before = response.allocated_before.unwrap_or(0);
        let after = response.allocated_after.unwrap_or(0);
        let logical = response.logical_bytes.unwrap_or(bytes);
        println!(
            "resized {vm}: allocated {} → {}, logical {}",
            human_size(before),
            human_size(after),
            human_size(logical)
        );
    }
    Ok(if response.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn vm_running(name: &str) -> Result<bool> {
    let socket = paths::socket_path(name)?;
    match client::connect(&socket)? {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

fn disk_path(vm: &str, disk: Option<u32>) -> Result<std::path::PathBuf> {
    let config_path = paths::vm_config_path(vm)?;
    let config = VmConfig::from_path(&config_path)
        .with_context(|| format!("load {}", config_path.display()))?;
    let index = disk.unwrap_or(0) as usize;
    let item = config
        .disks
        .get(index)
        .with_context(|| format!("no [[disks]] entry {index} for {vm}"))?;
    Ok(item.path.clone())
}

fn allocated_path(path: &Path) -> Result<u64> {
    if is_stack_manifest(path) {
        let stack = ImageStack::open(path, true).context("open stack for allocated size")?;
        return stack.allocated_bytes().context("allocated bytes");
    }
    let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    Ok(meta.blocks() * 512)
}

fn in_use_path(path: &Path) -> Result<u64> {
    if is_stack_manifest(path) {
        let stack = ImageStack::open(path, true).context("open stack for in-use size")?;
        return Ok(stack.in_use_bytes());
    }
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let len = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    raw_in_use(&file, len).with_context(|| format!("seek data in {}", path.display()))
}

fn raw_in_use(file: &File, len: u64) -> std::io::Result<u64> {
    if len == 0 {
        return Ok(0);
    }
    let mut pos = 0i64;
    let mut last = 0u64;
    loop {
        let data = match lseek(file.as_fd(), pos, Whence::SeekData) {
            Ok(pos) => pos,
            Err(nix::errno::Errno::ENXIO) => break,
            Err(error) => return Err(std::io::Error::from(error)),
        };
        let hole = match lseek(file.as_fd(), data, Whence::SeekHole) {
            Ok(pos) => pos as u64,
            Err(error) => return Err(std::io::Error::from(error)),
        };
        last = hole;
        if last >= len {
            last = len;
            break;
        }
        pos = hole as i64;
    }
    Ok(last)
}

fn print_sizes(json: bool, before: u64, after: u64, logical: Option<u64>) -> Result<()> {
    if json {
        let mut map = serde_json::json!({
            "ok": true,
            "allocated_before": before,
            "allocated_after": after,
        });
        if let Some(logical) = logical {
            map["logical_bytes"] = serde_json::json!(logical);
        }
        println!("{}", serde_json::to_string(&map).context("encode sizes")?);
    } else {
        match logical {
            Some(logical) => println!(
                "allocated {} → {}, logical {}",
                human_size(before),
                human_size(after),
                human_size(logical)
            ),
            None => println!("allocated {} → {}", human_size(before), human_size(after)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_unknown_vm_fails() {
        let err = compact("no-such-ternvale-vm-compact", None, false).expect_err("config");
        assert!(!err.to_string().is_empty());
    }
}
