//! `[vsock]` for `ternvale run`: the socket directory, the agent server, and
//! the device config handed to the attach step.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use ternvale_agent_proto::AGENT_PORT;
use ternvale_config::{VmConfig, MAX_UDS_DIR};
use ternvale_devices::{host_socket_path, AgentServer, VsockConfig};

use crate::paths;

/// What `ternvale run` needs for a VM with `[vsock]`.
pub struct VsockSetup {
    /// Device config for `VirtioVsock::attach`.
    pub device: VsockConfig,
    /// The agent server, unless `agent = false`.
    pub agent: Option<Arc<AgentServer>>,
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
    let agent = if section.agent {
        let path = host_socket_path(&dir, AGENT_PORT);
        let server = AgentServer::start(&path, &config.name)
            .with_context(|| format!("start the agent server on {}", path.display()))?;
        Some(Arc::new(server))
    } else {
        tracing::info!(target: "ternvale::cli", "agent server disabled (vsock.agent = false)");
        None
    };
    tracing::info!(target: "ternvale::cli", dir = %dir.display(), cid = ?section.cid, agent = section.agent, "vsock prepared");
    Ok(Some(VsockSetup {
        device: VsockConfig {
            guest_cid: section.cid,
            uds_dir: dir,
            listen_ports: section.listen_ports.clone(),
        },
        agent,
    }))
}
