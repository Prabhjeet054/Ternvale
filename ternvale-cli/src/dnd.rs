//! Finder ↔ guest drag-and-drop: sanitize, stage, confirm, copy, cleanup.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ternvale_config::{sanitize_file_name, unique_path, ClipboardSection, VmConfig};
use ternvale_devices::AgentServer;

use crate::paths;
use crate::protocol::{Request, Response};

const COPY_WAIT: Duration = Duration::from_secs(30);

/// One inbound or outbound file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferFile {
    /// Sanitized base name.
    pub name: String,
    /// Host path (inbound) or staged path (outbound).
    pub host_path: PathBuf,
    /// Size in bytes.
    pub bytes: u64,
}

/// Plan a drop from Finder into the guest (or a host analog dest).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, count = paths.len()))]
pub fn plan_inbound(
    vm: &str,
    paths: &[PathBuf],
    policy: &ClipboardSection,
) -> Result<Vec<TransferFile>> {
    if !policy.dnd {
        bail!("drag-and-drop is disabled (set clipboard.dnd = true)");
    }
    if paths.len() as u32 > policy.max_files {
        bail!("too many files (max {})", policy.max_files);
    }
    let mut files = Vec::new();
    for path in paths {
        collect_files(path, &mut files, policy.max_files)?;
    }
    if files.len() as u32 > policy.max_files {
        bail!("too many files (max {})", policy.max_files);
    }
    let mut out = Vec::new();
    for path in files {
        let meta = fs::metadata(&path).with_context(|| format!("stat {}", path.display()))?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed");
        let safe = sanitize_file_name(name);
        out.push(TransferFile {
            name: safe,
            host_path: path,
            bytes: meta.len(),
        });
    }
    let total: u64 = out.iter().map(|f| f.bytes).sum();
    if total > policy.max_bytes() {
        bail!(
            "transfer {} bytes exceeds clipboard.max_mib {}",
            total,
            policy.max_mib
        );
    }
    tracing::info!(
        target: "ternvale::cli",
        vm,
        files = out.len(),
        bytes = total,
        "dnd inbound planned"
    );
    Ok(out)
}

/// Walk `path` (file or directory) into `out`, refusing more than `cap` files.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn collect_files(path: &Path, out: &mut Vec<PathBuf>, cap: u32) -> Result<()> {
    let meta = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if meta.is_dir() {
        for entry in fs::read_dir(path).with_context(|| format!("read_dir {}", path.display()))? {
            let entry = entry.with_context(|| format!("read_dir {}", path.display()))?;
            collect_files(&entry.path(), out, cap)?;
            if out.len() as u32 > cap {
                bail!("too many files (max {cap})");
            }
        }
        return Ok(());
    }
    if out.len() as u32 >= cap {
        bail!("too many files (max {cap})");
    }
    out.push(path.to_path_buf());
    Ok(())
}

/// True when the UI must prompt before copying.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn needs_size_confirm(files: &[TransferFile], policy: &ClipboardSection) -> bool {
    let total: u64 = files.iter().map(|f| f.bytes).sum();
    total > policy.confirm_bytes()
}

/// Per-VM staging directory (`<vm_dir>/dnd-stage/<id>/`).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, id))]
pub fn stage_dir(vm: &str, id: &str) -> Result<PathBuf> {
    let dir = paths::vm_dir(vm)?.join("dnd-stage").join(id);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

/// Copy host files into the stage dir with unique names (never overwrite).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, id))]
pub fn stage_host_files(vm: &str, id: &str, files: &[TransferFile]) -> Result<Vec<PathBuf>> {
    let dir = stage_dir(vm, id)?;
    let mut staged = Vec::new();
    for file in files {
        let dest = unique_path(&dir, &file.name);
        fs::copy(&file.host_path, &dest)
            .with_context(|| format!("stage {} -> {}", file.host_path.display(), dest.display()))?;
        tracing::debug!(
            target: "ternvale::cli",
            src = %file.host_path.display(),
            dest = %dest.display(),
            bytes = file.bytes,
            "staged dnd file"
        );
        staged.push(dest);
    }
    Ok(staged)
}

/// Remove a transfer's stage directory.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, id))]
pub fn cleanup(vm: &str, id: &str) -> Result<()> {
    let dir = paths::vm_dir(vm)?.join("dnd-stage").join(id);
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
        tracing::info!(target: "ternvale::cli", vm, id, path = %dir.display(), "dnd stage cleaned");
    }
    Ok(())
}

/// Guest path for a drop (`desktop` / `downloads` / `documents`).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn guest_drop_path(policy: &ClipboardSection, windows: bool) -> String {
    match (windows, policy.guest_drop.as_str()) {
        (true, "downloads") => r"C:\Users\Public\Downloads".into(),
        (true, "documents") => r"C:\Users\Public\Documents".into(),
        (true, _) => r"C:\Users\Public\Desktop".into(),
        (false, "downloads") => "/tmp/ternvale-drop/Downloads".into(),
        (false, "documents") => "/tmp/ternvale-drop/Documents".into(),
        (false, _) => "/tmp/ternvale-drop/Desktop".into(),
    }
}

/// Push staged files into the guest over the copy channel (checksum after).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(id, count = staged.len()))]
pub fn put_to_guest(
    agent: &AgentServer,
    staged: &[PathBuf],
    guest_dir: &str,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<u64> {
    let mut total = 0u64;
    let sep = if guest_dir.contains('\\') { '\\' } else { '/' };
    for path in staged {
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            bail!("transfer cancelled");
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed");
        let guest = format!("{guest_dir}{sep}{name}");
        let data = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let mut offset = 0u64;
        while offset < data.len() as u64 {
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                bail!("transfer cancelled");
            }
            let start = offset as usize;
            let end = (start + ternvale_agent_proto::MAX_BIN_CHUNK).min(data.len());
            let last = end == data.len();
            agent
                .copy_put(&guest, offset, &data[start..end], last, COPY_WAIT)
                .with_context(|| format!("copy_put {guest}"))?;
            offset = end as u64;
        }
        let (size, sha) = agent
            .copy_hash(&guest, COPY_WAIT)
            .with_context(|| format!("copy_hash {guest}"))?;
        tracing::info!(
            target: "ternvale::cli",
            guest = %guest,
            size,
            sha256 = %sha,
            "dnd file verified"
        );
        total += size;
    }
    Ok(total)
}

/// Control-socket handler.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn handle(request: &Request, agent: Option<&AgentServer>, vm: &str) -> Option<Response> {
    match request {
        Request::DragIn {
            paths,
            dest,
            confirm,
            id,
        } => Some(drag_in(
            agent,
            vm,
            paths,
            dest.as_deref(),
            *confirm,
            id.as_deref(),
        )),
        Request::DragOutPromise { names, sizes, id } => {
            Some(drag_out_promise(vm, names, sizes, id.as_deref()))
        }
        Request::DragCancel { id } => Some(drag_cancel(vm, id)),
        Request::ClipboardOffer {
            formats,
            confirm,
            dest: _,
        } => Some(clipboard_offer(agent, vm, formats, *confirm)),
        _ => None,
    }
}

fn policy(vm: &str) -> Result<ClipboardSection> {
    let path = paths::vm_config_path(vm)?;
    match VmConfig::from_path(&path) {
        Ok(config) => Ok(config.clipboard.unwrap_or_default()),
        Err(_) => Ok(ClipboardSection::default()),
    }
}

fn drag_in(
    agent: Option<&AgentServer>,
    vm: &str,
    paths: &[String],
    dest: Option<&str>,
    confirm: bool,
    id: Option<&str>,
) -> Response {
    let policy = match policy(vm) {
        Ok(p) => p,
        Err(error) => return Response::error(error.to_string()),
    };
    let host_paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let files = match plan_inbound(vm, &host_paths, &policy) {
        Ok(files) => files,
        Err(error) => return Response::error(format!("{error:#}")),
    };
    if needs_size_confirm(&files, &policy) && !confirm {
        return Response::needs_confirmation("dnd-size");
    }
    let id = id.unwrap_or("drop").to_string();
    let staged = match stage_host_files(vm, &id, &files) {
        Ok(s) => s,
        Err(error) => return Response::error(format!("{error:#}")),
    };
    let mut response = Response::ok();
    response.drag_id = Some(id.clone());
    response.staged = Some(staged.iter().map(|p| p.display().to_string()).collect());
    response.bytes = Some(files.iter().map(|f| f.bytes).sum());
    response.formats = Some(vec!["files".into()]);
    if let Some(agent) = agent {
        let windows = dest.map(|d| d.contains(':')).unwrap_or(false);
        let guest_dir = dest
            .map(str::to_string)
            .unwrap_or_else(|| guest_drop_path(&policy, windows));
        let cancel = std::sync::atomic::AtomicBool::new(false);
        match put_to_guest(agent, &staged, &guest_dir, &cancel) {
            Ok(bytes) => {
                response.bytes = Some(bytes);
                let _ = agent.send(&ternvale_agent_proto::Message::DragOffer {
                    id: id.clone(),
                    names: files.iter().map(|f| f.name.clone()).collect(),
                    sizes: files.iter().map(|f| f.bytes).collect(),
                    outbound: false,
                    echo: 0,
                });
            }
            Err(error) => {
                let _ = cleanup(vm, &id);
                return Response::error(format!("drag-in: {error:#}"));
            }
        }
    }
    response
}

fn drag_out_promise(vm: &str, names: &[String], sizes: &[u64], id: Option<&str>) -> Response {
    let policy = match policy(vm) {
        Ok(p) => p,
        Err(error) => return Response::error(error.to_string()),
    };
    if !policy.dnd {
        return Response::error("drag-and-drop is disabled (set clipboard.dnd = true)");
    }
    if names.len() as u32 > policy.max_files {
        return Response::error(format!("too many files (max {})", policy.max_files));
    }
    let total: u64 = sizes.iter().copied().sum();
    if total > policy.max_bytes() {
        return Response::error("transfer exceeds clipboard.max_mib");
    }
    if total > policy.confirm_bytes() {
        return Response::needs_confirmation("dnd-size");
    }
    let id = id.unwrap_or("out").to_string();
    if let Err(error) = stage_dir(vm, &id) {
        return Response::error(error.to_string());
    }
    let mut response = Response::ok();
    response.drag_id = Some(id);
    response.formats = Some(names.iter().map(|n| sanitize_file_name(n)).collect());
    response.bytes = Some(total);
    response
}

fn drag_cancel(vm: &str, id: &str) -> Response {
    match cleanup(vm, id) {
        Ok(()) => {
            let mut response = Response::ok();
            response.drag_id = Some(id.to_string());
            response
        }
        Err(error) => Response::error(error.to_string()),
    }
}

fn clipboard_offer(
    agent: Option<&AgentServer>,
    vm: &str,
    formats: &[String],
    confirm: bool,
) -> Response {
    let policy = match policy(vm) {
        Ok(p) => p,
        Err(error) => return Response::error(error.to_string()),
    };
    if !policy.rich {
        return Response::error("rich clipboard is disabled (set clipboard.rich = true)");
    }
    let Some(format) = ternvale_agent_proto::negotiate_format(formats) else {
        return Response::error("no overlapping clipboard format");
    };
    let _ = confirm;
    if let Some(agent) = agent {
        let seq = 1u64;
        if let Err(error) = agent.send(&ternvale_agent_proto::Message::ClipboardOffer {
            seq,
            formats: vec![format.to_string()],
            sizes: Vec::new(),
            files: Vec::new(),
            echo: seq,
        }) {
            return Response::error(error.to_string());
        }
    }
    let mut response = Response::ok();
    response.formats = Some(vec![format.to_string()]);
    response
}

/// Host analog: sanitize + unique dest without a VM.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn analog_drop(dest_dir: &Path, raw_names: &[&str]) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(dest_dir).context("create analog dest")?;
    let mut out = Vec::new();
    for name in raw_names {
        let dest = unique_path(dest_dir, name);
        fs::write(&dest, name.as_bytes()).with_context(|| format!("write {}", dest.display()))?;
        out.push(dest);
    }
    Ok(out)
}

#[cfg(test)]
#[path = "dnd_quality.rs"]
mod quality;
