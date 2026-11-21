//! `[vsock]` for `ternvale run`: the socket directory, the agent server, and
//! the device config handed to the attach step.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use ternvale_agent_proto::{AGENT_PORT, IPP_PORT};
use ternvale_config::{VmConfig, MAX_UDS_DIR};
use ternvale_devices::{
    host_socket_path, list_host_queues, open_preview, AgentServer, AgentServerConfig, PrintPolicy,
    PrintProxy, VsockConfig,
};

use crate::paths;

/// What `ternvale run` needs for a VM with `[vsock]`.
pub struct VsockSetup {
    /// Device config for `VirtioVsock::attach`.
    pub device: VsockConfig,
    /// The agent server, unless `agent = false`.
    pub agent: Option<Arc<AgentServer>>,
    /// Attach virtio-vsock. False uses virtio-console only.
    pub attach_vsock: bool,
    /// Attach virtio-console named port `org.ternvale.agent`.
    pub attach_serial: bool,
    /// IPP proxy on `host-8631.sock` (kept alive for the VM).
    pub print: Option<Arc<PrintProxy>>,
}

/// `<dir of socket>/<name>.vsock`: the default socket directory, beside the
/// control socket.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name))]
pub fn default_uds_dir_beside(control_socket: &Path, name: &str) -> PathBuf {
    control_socket.with_file_name(format!("{name}.vsock"))
}

/// Create the socket directory (mode 0700) and start the agent server.
/// `Ok(None)` when the config has no `[vsock]`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = %config.name))]
pub fn prepare(config: &VmConfig, control_socket: &Path) -> Result<Option<VsockSetup>> {
    let Some(section) = &config.vsock else {
        tracing::debug!(target: "ternvale::cli", "no [vsock] section; no vsock device or agent");
        return Ok(None);
    };
    let dir = match &section.uds_dir {
        Some(dir) => dir.clone(),
        None => default_uds_dir_beside(control_socket, &config.name),
    };
    let len = dir.as_os_str().len();
    if len > MAX_UDS_DIR {
        tracing::warn!(target: "ternvale::cli", dir = %dir.display(), len, max = MAX_UDS_DIR, "vsock socket directory too long");
        bail!(
            "vsock socket directory {} is {len} bytes; macOS socket paths allow {MAX_UDS_DIR} here (set vsock.uds_dir or {})",
            dir.display(),
            paths::RUN_DIR_ENV
        );
    }
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create vsock socket directory {}", dir.display()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restrict vsock socket directory {}", dir.display()))?;
    let shares: Vec<(String, String)> = config
        .shares
        .iter()
        .map(|share| (share.tag.clone(), share.mount_point.clone()))
        .collect();
    let agent = if section.agent {
        let path = host_socket_path(&dir, AGENT_PORT);
        let server =
            AgentServer::start_with(&path, &config.name, AgentServerConfig::default(), shares)
                .with_context(|| format!("start the agent server on {}", path.display()))?;
        server.set_clipboard_sink(Arc::new(|text| {
            if let Err(error) = crate::clipboard::set_host_text(&text) {
                tracing::warn!(
                    target: "ternvale::cli",
                    error = %format!("{error:#}"),
                    "guest clipboard could not update the host pasteboard"
                );
            }
        }));
        let policy = config.agent.clone();
        let share_list = config.shares.clone();
        let vm_name = config.name.clone();
        server.set_host_open_sink(Arc::new(move |kind, target| {
            crate::assoc::handle_guest_open(&vm_name, &policy, &share_list, &kind, &target)
        }));
        if let Some(section) = &config.print {
            let queues = list_host_queues();
            let pdf_dir = print_pdf_dir(&config.name);
            let policy = PrintPolicy::from_config(
                &config.name,
                section,
                &queues,
                pdf_dir,
                Arc::new(open_preview),
            );
            if section.enabled {
                server.set_printers(Some(policy.guest_printers()));
            } else {
                server.set_printers(Some(Vec::new()));
            }
        }
        Some(Arc::new(server))
    } else {
        tracing::info!(target: "ternvale::cli", "agent server disabled (vsock.agent = false)");
        None
    };
    let print = match (config.print.as_ref(), section.agent) {
        (Some(print_section), _) if print_section.enabled => {
            let queues = list_host_queues();
            let policy = PrintPolicy::from_config(
                &config.name,
                print_section,
                &queues,
                print_pdf_dir(&config.name),
                Arc::new(open_preview),
            );
            let path = host_socket_path(&dir, IPP_PORT);
            match PrintProxy::start(&path, policy) {
                Ok(proxy) => {
                    tracing::info!(
                        target: "ternvale::print",
                        path = %path.display(),
                        "print proxy started"
                    );
                    Some(Arc::new(proxy))
                }
                Err(error) => {
                    tracing::error!(
                        target: "ternvale::print",
                        error = %error,
                        "print proxy failed to start"
                    );
                    return Err(error).context("start IPP print proxy");
                }
            }
        }
        _ => None,
    };
    tracing::info!(
        target: "ternvale::cli",
        dir = %dir.display(),
        cid = ?section.cid,
        agent = section.agent,
        serial = section.serial,
        vsock_device = section.device,
        print = print.is_some(),
        "vsock prepared"
    );
    Ok(Some(VsockSetup {
        device: VsockConfig {
            guest_cid: section.cid,
            uds_dir: dir,
            listen_ports: section.listen_ports.clone(),
        },
        agent,
        attach_vsock: section.device,
        attach_serial: section.serial && section.agent,
        print,
    }))
}

fn print_pdf_dir(vm: &str) -> std::path::PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into());
    std::path::PathBuf::from(home)
        .join("Library/Application Support/Ternvale")
        .join(vm)
        .join("print-pdf")
}
