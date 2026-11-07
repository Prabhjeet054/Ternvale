//! `ternvale tools`: guest tools ISO (`ternvale-tools.iso`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use ternvale_config::{Profile, VmConfig};

use crate::protocol::StatusJson;
use crate::tools_build::{self, sidecar_path, BuildIsoArgs};
use crate::tools_manifest::ToolsManifest;

/// Env: directory that holds `ternvale-tools.iso`.
pub const TOOLS_DIR_ENV: &str = "TERNVALE_TOOLS_DIR";
/// Env: exact path of the tools ISO (overrides the directory default).
pub const TOOLS_ISO_ENV: &str = "TERNVALE_TOOLS_ISO";

/// `ternvale tools …`
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum ToolsCommand {
    /// Assemble `ternvale-tools.iso` (virtio-win + WinFsp MSI + agent + install.ps1).
    BuildIso {
        /// virtio-win ISO or an extracted directory.
        #[arg(long)]
        virtio_win: Option<PathBuf>,
        /// Run `scripts/fetch-virtio-win.sh` and `scripts/fetch-winfsp.sh` when omitted.
        #[arg(long)]
        fetch: bool,
        /// Windows agent PE (`ternvale-agent.exe`).
        #[arg(long)]
        agent: Option<PathBuf>,
        /// Output ISO path.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Override `agent_version` written into MANIFEST.json.
        #[arg(long)]
        agent_version: Option<String>,
    },
    /// Check file SHA-256s of a mounted tools volume against the sidecar manifest.
    Verify {
        /// Mounted volume root (or extracted tree).
        root: PathBuf,
        /// Tools ISO whose sidecar to read (default: `TERNVALE_TOOLS_ISO`).
        #[arg(long)]
        iso: Option<PathBuf>,
    },
    /// Compare a guest agent Hello string to the tools ISO manifest.
    Outdated {
        /// Tools ISO (default: `TERNVALE_TOOLS_ISO` / Application Support).
        #[arg(long)]
        iso: Option<PathBuf>,
        /// Guest Hello `agent` field, e.g. `ternvale-agent 0.1.0`.
        #[arg(long)]
        guest_agent: String,
    },
}

/// Default directory for the tools ISO.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn tools_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(TOOLS_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }
    let home =
        std::env::var_os("HOME").context("HOME is not set; cannot find the tools directory")?;
    Ok(PathBuf::from(home).join("Library/Application Support/Ternvale/tools"))
}

/// Default `ternvale-tools.iso` path (`TERNVALE_TOOLS_ISO` or `tools_dir()`).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn default_tools_iso() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(TOOLS_ISO_ENV) {
        return Ok(PathBuf::from(path));
    }
    Ok(tools_dir()?.join("ternvale-tools.iso"))
}

/// For `profile = "windows"`, attach the default tools ISO as `iso.tools`
/// when the slot is empty and the file exists. Does not override `iso.drivers`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = %config.name, profile = %config.profile))]
pub fn auto_attach_windows_tools(config: &mut VmConfig) {
    if config.profile != Profile::Windows {
        return;
    }
    let Ok(path) = default_tools_iso() else {
        return;
    };
    if !path.is_file() {
        tracing::debug!(
            target: "ternvale::cli",
            path = %path.display(),
            "no tools ISO on disk; skipping auto-attach"
        );
        return;
    }
    let iso = config.iso.get_or_insert_with(Default::default);
    if iso.tools.is_some() {
        return;
    }
    if iso
        .drivers
        .as_ref()
        .is_some_and(|drivers| same_path(drivers, &path))
    {
        tracing::debug!(
            target: "ternvale::cli",
            path = %path.display(),
            "iso.drivers already points at the tools ISO"
        );
        return;
    }
    tracing::info!(
        target: "ternvale::cli",
        path = %path.display(),
        "auto-attaching ternvale-tools.iso as iso.tools (second optical drive)"
    );
    iso.tools = Some(path);
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// `ternvale tools build-iso`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn tools(command: ToolsCommand) -> Result<ExitCode> {
    match command {
        ToolsCommand::BuildIso {
            virtio_win,
            fetch,
            agent,
            out,
            agent_version,
        } => {
            let out = match out {
                Some(path) => path,
                None => default_tools_iso()?,
            };
            let built = tools_build::build_iso(BuildIsoArgs {
                virtio_win,
                fetch,
                agent,
                out,
                agent_version,
            })?;
            println!("wrote {}", built.iso.display());
            println!("manifest {}", built.sidecar.display());
            println!("tools_version {}", built.manifest.tools_version);
            println!("agent_version {}", built.manifest.agent_version);
            Ok(ExitCode::SUCCESS)
        }
        ToolsCommand::Verify { root, iso } => {
            let iso = match iso {
                Some(path) => path,
                None => default_tools_iso()?,
            };
            let sidecar = sidecar_path(&iso);
            let json = std::fs::read_to_string(&sidecar)
                .with_context(|| format!("read {}", sidecar.display()))?;
            let manifest = ToolsManifest::parse(&json).map_err(|error| anyhow!("{error}"))?;
            crate::tools_verify::verify_tree(&manifest, &root)?;
            println!(
                "verified {} files under {}",
                manifest.files.len(),
                root.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        ToolsCommand::Outdated { iso, guest_agent } => {
            let iso = match iso {
                Some(path) => path,
                None => default_tools_iso()?,
            };
            let (outdated, expected) = guest_tools_outdated(&iso, &guest_agent)?;
            println!("iso {}", iso.display());
            println!("iso_agent_version {expected}");
            println!("guest_agent {guest_agent}");
            println!("outdated {outdated}");
            if outdated {
                Ok(ExitCode::from(2))
            } else {
                Ok(ExitCode::SUCCESS)
            }
        }
    }
}

/// Load the sidecar (or fail clearly) and compare to a guest Hello agent string.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(iso = %iso.display()))]
pub fn guest_tools_outdated(iso: &Path, guest_agent: &str) -> Result<(bool, String)> {
    let sidecar = sidecar_path(iso);
    let json = std::fs::read_to_string(&sidecar)
        .with_context(|| format!("read tools manifest {}", sidecar.display()))?;
    let manifest = ToolsManifest::parse(&json).map_err(|error| anyhow!("{error}"))?;
    let outdated = manifest.guest_outdated(guest_agent);
    Ok((outdated, manifest.agent_version))
}

/// Fill optional `tools_outdated` on a control-socket status (existing fields unchanged).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn fill_tools_outdated(status: &mut StatusJson, tools_iso: Option<&Path>) {
    let Some(iso) = tools_iso else {
        return;
    };
    let guest = status
        .agent
        .as_ref()
        .and_then(|agent| agent.agent.as_deref())
        .unwrap_or("");
    match guest_tools_outdated(iso, guest) {
        Ok((outdated, expected)) => {
            tracing::info!(
                target: "ternvale::cli",
                iso = %iso.display(),
                guest,
                expected = %expected,
                outdated,
                "compared guest agent to tools ISO"
            );
            status.tools_outdated = Some(outdated);
            status.tools_iso_agent = Some(expected);
        }
        Err(error) => tracing::warn!(
            target: "ternvale::cli",
            iso = %iso.display(),
            error = %error,
            "could not compare guest tools to the ISO manifest"
        ),
    }
}
