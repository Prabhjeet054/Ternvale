//! `ternvale snapshot create|restore|list|delete`.

use std::process::ExitCode;

use anyhow::{Context, Result};
use ternvale_snapshot::{delete_bundle, list_snapshots};

use crate::client::{self, Reach};
use crate::paths;
use crate::protocol::{Request, Response};

/// `ternvale snapshot create <vm> <name>`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, label))]
pub fn create(vm: &str, label: &str, json: bool) -> Result<ExitCode> {
    let socket = paths::socket_path(vm)?;
    let response = client::request(
        &socket,
        vm,
        &Request::SnapshotCreate {
            name: label.to_string(),
        },
    )
    .with_context(|| format!("snapshot create {label} for vm {vm}"))?;
    print_snapshot_response(&response, json)?;
    Ok(ExitCode::SUCCESS)
}

/// `ternvale snapshot restore <vm> <name>`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, label))]
pub fn restore(vm: &str, label: &str, json: bool) -> Result<ExitCode> {
    let socket = paths::socket_path(vm)?;
    let response = client::request(
        &socket,
        vm,
        &Request::SnapshotRestore {
            name: label.to_string(),
        },
    )
    .with_context(|| format!("snapshot restore {label} for vm {vm}"))?;
    print_snapshot_response(&response, json)?;
    Ok(ExitCode::SUCCESS)
}

/// `ternvale snapshot list <vm>` — uses the control socket when the VM is running,
/// otherwise reads the snapshot store on disk.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm))]
pub fn list(vm: &str, json: bool) -> Result<ExitCode> {
    let socket = paths::socket_path(vm)?;
    let response = match client::connect(&socket)? {
        Ok(_) => client::request(&socket, vm, &Request::SnapshotList)
            .with_context(|| format!("snapshot list for vm {vm}"))?,
        Err(Reach::NotRunning) => {
            let root = paths::snapshots_root()?;
            let names = list_snapshots(&root, vm)
                .with_context(|| format!("list snapshots under {}", root.join(vm).display()))?;
            Response::snapshots(
                &ternvale_vmm::VmStatus {
                    name: vm.to_string(),
                    state: ternvale_vmm::VmState::Stopped,
                    cpus: 0,
                    uptime_ms: 0,
                    state_ms: 0,
                    paused_ms: 0,
                    pauses: 0,
                    stop_cause: None,
                    failure: None,
                },
                names,
                None,
            )
        }
        Err(Reach::Stale) => {
            anyhow::bail!("control socket for {vm} exists but is not accepting connections")
        }
    };
    print_snapshot_response(&response, json)?;
    Ok(ExitCode::SUCCESS)
}

/// `ternvale snapshot delete <vm> <name>`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, label))]
pub fn delete(vm: &str, label: &str, json: bool) -> Result<ExitCode> {
    let socket = paths::socket_path(vm)?;
    let response = match client::connect(&socket)? {
        Ok(_) => client::request(
            &socket,
            vm,
            &Request::SnapshotDelete {
                name: label.to_string(),
            },
        )
        .with_context(|| format!("snapshot delete {label} for vm {vm}"))?,
        Err(Reach::NotRunning) => {
            let bundle = paths::snapshot_bundle(vm, label)?;
            delete_bundle(&bundle).with_context(|| format!("delete {}", bundle.display()))?;
            Response::snapshots(
                &ternvale_vmm::VmStatus {
                    name: vm.to_string(),
                    state: ternvale_vmm::VmState::Stopped,
                    cpus: 0,
                    uptime_ms: 0,
                    state_ms: 0,
                    paused_ms: 0,
                    pauses: 0,
                    stop_cause: None,
                    failure: None,
                },
                Vec::new(),
                None,
            )
        }
        Err(Reach::Stale) => {
            anyhow::bail!("control socket for {vm} exists but is not accepting connections")
        }
    };
    print_snapshot_response(&response, json)?;
    Ok(ExitCode::SUCCESS)
}

fn print_snapshot_response(response: &Response, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(response).expect("json"));
        if !response.ok {
            anyhow::bail!("{}", response.error.as_deref().unwrap_or("request failed"));
        }
        return Ok(());
    }
    if !response.ok {
        anyhow::bail!("{}", response.error.as_deref().unwrap_or("request failed"));
    }
    if let Some(path) = &response.snapshot_path {
        println!("snapshot: {}", path);
    }
    if let Some(names) = &response.snapshots {
        if names.is_empty() {
            println!("(no snapshots)");
        } else {
            for name in names {
                println!("{name}");
            }
        }
    }
    Ok(())
}
