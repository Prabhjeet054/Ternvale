//! `ternvale apps` — list/launch/stubs/open/approve.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ternvale_config::{file_ext, host_file_to_guest, is_blocked_exec_ext, VmConfig};

use crate::json_out;
use crate::protocol::{AppJson, Request, Response};
use crate::{assoc, stubs};

const AGENT_WAIT: Duration = Duration::from_secs(5);

/// `ternvale apps list <vm>`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm))]
pub fn list(vm: &str, json: bool) -> Result<ExitCode> {
    let response = list_response(vm)?;
    emit(json, &response, || {
        if let Some(apps) = &response.apps {
            if apps.is_empty() {
                println!("no Start-menu apps (is the guest agent connected?)");
            }
            for app in apps {
                println!("{}\t{}\t{}", app.id, app.name, app.source);
            }
        }
    })
}

/// `ternvale apps launch`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, id))]
pub fn launch(vm: &str, id: &str, files: &[String], confirm: bool, json: bool) -> Result<ExitCode> {
    let response = launch_response(vm, id, files, confirm)?;
    emit(json, &response, || {
        if response.ok {
            println!("launched {id} on {vm}");
        }
    })
}

/// `ternvale apps install-stubs`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm))]
pub fn install_stubs(vm: &str, json: bool) -> Result<ExitCode> {
    let response = install_stubs_response(vm)?;
    let dir = stubs::vm_apps_dir(vm)?;
    emit(json, &response, || {
        let n = response.stubs_removed.unwrap_or(0);
        println!("installed {n} launcher(s) in {}", dir.display());
    })
}

/// Hub `apps.stubs.install`.
#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm))]
pub fn install_stubs_response(vm: &str) -> Result<Response> {
    let config = load_config(vm)?;
    if !config.agent.launchers {
        return Ok(Response::error(
            "launcher stubs are disabled (set agent.launchers = true)",
        ));
    }
    let apps = guest_apps(vm)?;
    let bin = std::env::current_exe().context("current_exe")?;
    let written = stubs::install(vm, &apps, &bin)?;
    let mut response = Response::ok();
    response.stubs_removed = Some(u64::from(written));
    Ok(response)
}

/// `ternvale apps uninstall-stubs`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = vm.unwrap_or("*")))]
pub fn uninstall_stubs(vm: Option<&str>, json: bool) -> Result<ExitCode> {
    let removed = stubs::uninstall(vm)?;
    let mut response = Response::ok();
    response.stubs_removed = Some(u64::from(removed));
    emit(json, &response, || {
        println!("removed {removed} launcher stub(s)");
    })
}

/// `ternvale apps open` (Finder Open With).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, id))]
pub fn open_in_guest(
    vm: &str,
    id: &str,
    host_file: &Path,
    confirm: bool,
    json: bool,
) -> Result<ExitCode> {
    let response = open_response(vm, id, host_file, confirm)?;
    emit(json, &response, || {
        if response.ok {
            println!("opened {} in {id} on {vm}", host_file.display());
        }
    })
}

/// `ternvale apps approve`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm, key))]
pub fn approve(vm: &str, key: &str, json: bool) -> Result<ExitCode> {
    assoc::approve(vm, key)?;
    emit(json, &Response::ok(), || {
        println!("approved {key} for {vm}");
    })
}

/// Hub `apps.list`.
#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm))]
pub fn list_response(vm: &str) -> Result<Response> {
    let apps = guest_apps(vm)?;
    Ok(Response::apps(
        apps.into_iter().map(AppJson::from).collect(),
    ))
}

/// Hub `apps.launch` / stub entry.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::api",
    skip_all,
    fields(vm, id, confirm)
)]
pub fn launch_response(vm: &str, id: &str, files: &[String], confirm: bool) -> Result<Response> {
    let config = load_config(vm)?;
    if !config.agent.launchers {
        return Ok(Response::error(
            "guest app launchers are disabled (set agent.launchers = true)",
        ));
    }
    let key = if files.is_empty() {
        format!("guest-app:{id}")
    } else {
        let ext = files
            .first()
            .map(|f| file_ext(Path::new(f)))
            .unwrap_or_default();
        format!("guest-open:{id}:{ext}")
    };
    if assoc::needs_confirmation(vm, &key, confirm)? {
        return Ok(Response::needs_confirmation(&key));
    }
    ensure_running_seamless(vm)?;
    wait_agent(vm, 120)?;
    let guest_file = if files.is_empty() {
        None
    } else {
        if !config.agent.open_in_guest {
            return Ok(Response::error(
                "open in guest is disabled (set agent.open_in_guest = true)",
            ));
        }
        if files.len() != 1 {
            bail!("open in guest takes exactly one host file (share path, never copied)");
        }
        let host = Path::new(&files[0]);
        if is_blocked_exec_ext(&file_ext(host)) {
            return Ok(Response::error(
                "refusing to auto-execute this file type in the guest",
            ));
        }
        Some(host_file_to_guest(&config.shares, host).map_err(|e| anyhow::anyhow!(e))?)
    };
    crate::client::request(
        &crate::paths::socket_path(vm)?,
        vm,
        &Request::AppLaunch {
            id: id.to_string(),
            file: guest_file,
            confirm: true,
        },
    )
}

/// Hub `apps.open`.
#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm, id))]
pub fn open_response(vm: &str, id: &str, host_file: &Path, confirm: bool) -> Result<Response> {
    launch_response(vm, id, &[host_file.display().to_string()], confirm)
}

fn guest_apps(vm: &str) -> Result<Vec<ternvale_agent_proto::GuestApp>> {
    if !crate::api_library::vm_is_running(vm)? {
        bail!("VM {vm} is not running; start it to list Start-menu apps");
    }
    let response = crate::client::request(&crate::paths::socket_path(vm)?, vm, &Request::AppList)?;
    if !response.ok {
        bail!(
            "{}",
            response.error.unwrap_or_else(|| "app-list failed".into())
        );
    }
    Ok(response
        .apps
        .unwrap_or_default()
        .into_iter()
        .map(|app| ternvale_agent_proto::GuestApp {
            id: app.id,
            name: app.name,
            exe: app.exe,
            source: match app.source.as_str() {
                "packaged" => ternvale_agent_proto::AppSource::Packaged,
                _ => ternvale_agent_proto::AppSource::StartMenu,
            },
            extensions: app.extensions,
            icon: None,
        })
        .collect())
}

/// Env set when a Launchpad stub starts a stopped VM.
pub const SEAMLESS_ENV: &str = "TERNVALE_SEAMLESS_WINDOWS";

/// `ternvale run` invocation used when a stub finds the VM stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeamlessStart {
    /// `ternvale` binary.
    pub bin: std::path::PathBuf,
    /// VM `config.toml`.
    pub config: std::path::PathBuf,
}

impl SeamlessStart {
    /// Build the process (does not spawn).
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(bin = %self.bin.display(), config = %self.config.display()))]
    pub fn command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.bin);
        cmd.arg("run").arg(&self.config).env(SEAMLESS_ENV, "1");
        cmd
    }
}

fn ensure_running_seamless(vm: &str) -> Result<()> {
    if crate::api_library::vm_is_running(vm)? {
        return Ok(());
    }
    let start = SeamlessStart {
        config: crate::paths::vm_config_path(vm)?,
        bin: std::env::var_os("TERNVALE_BIN")
            .map(std::path::PathBuf::from)
            .unwrap_or(std::env::current_exe().context("current_exe")?),
    };
    start
        .command()
        .spawn()
        .with_context(|| format!("start VM {vm}"))?;
    tracing::info!(
        target: "ternvale::cli",
        vm,
        env = SEAMLESS_ENV,
        "started VM for guest app launch (seamless)"
    );
    Ok(())
}

fn wait_agent(vm: &str, timeout: u64) -> Result<()> {
    let socket = crate::paths::socket_path(vm)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout.max(1));
    loop {
        match crate::client::connect(&socket)? {
            Err(_) => {
                if std::time::Instant::now() >= deadline {
                    bail!("vm {vm} is not running");
                }
            }
            Ok(stream) => {
                let response = crate::client::exchange(stream, &Request::Status)
                    .with_context(|| format!("status vm {vm}"))?;
                let ready = response
                    .status
                    .as_ref()
                    .and_then(|s| s.agent.as_ref())
                    .is_some_and(|a| a.state == "connected");
                if ready {
                    return Ok(());
                }
                if std::time::Instant::now() >= deadline {
                    bail!("vm {vm} agent did not connect within {timeout}s");
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn load_config(vm: &str) -> Result<VmConfig> {
    let path = crate::paths::vm_config_path(vm)?;
    VmConfig::from_path(&path).with_context(|| format!("load {}", path.display()))
}

fn emit(json: bool, response: &Response, human: impl FnOnce()) -> Result<ExitCode> {
    if !response.ok {
        json_out::emit(
            json,
            serde_json::to_value(response).unwrap_or(serde_json::json!({"ok":false})),
            || {},
        )?;
        if let Some(error) = &response.error {
            if !json {
                eprintln!("{error}");
            }
        }
        return Ok(ExitCode::FAILURE);
    }
    json_out::emit(
        json,
        serde_json::to_value(response).unwrap_or(serde_json::json!({"ok":true})),
        human,
    )?;
    Ok(ExitCode::SUCCESS)
}

/// Control-socket handler (VM process).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn handle(
    request: &Request,
    agent: Option<&ternvale_devices::AgentServer>,
    launchers: bool,
) -> Option<Response> {
    match request {
        Request::AppList => Some(app_list(agent, launchers)),
        Request::AppLaunch { id, file, confirm } => {
            Some(app_launch(agent, launchers, id, file.as_deref(), *confirm))
        }
        _ => None,
    }
}

fn app_list(agent: Option<&ternvale_devices::AgentServer>, launchers: bool) -> Response {
    if !launchers {
        return Response::error("guest app launchers are disabled (set agent.launchers = true)");
    }
    let Some(agent) = agent else {
        return Response::error("this vm has no guest agent server");
    };
    match agent.app_list_get(AGENT_WAIT) {
        Ok(apps) => Response::apps(apps.into_iter().map(AppJson::from).collect()),
        Err(error) => Response::error(format!("app-list: {error}")),
    }
}

fn app_launch(
    agent: Option<&ternvale_devices::AgentServer>,
    launchers: bool,
    id: &str,
    file: Option<&str>,
    _confirm: bool,
) -> Response {
    if !launchers {
        return Response::error("guest app launchers are disabled (set agent.launchers = true)");
    }
    let Some(agent) = agent else {
        return Response::error("this vm has no guest agent server");
    };
    match agent.app_launch(id, file, AGENT_WAIT) {
        Ok(()) => Response::ok(),
        Err(error) => Response::error(format!("app-launch: {error}")),
    }
}

#[cfg(test)]
#[path = "apps_quality.rs"]
mod quality;
