//! Windows Service Control Manager host for `ternvale-agent`.
//!
//! `--install` registers `TernvaleGuestAgent` as auto-start with restart on
//! failure. `--service` is the SCM entry (do not run it by hand).

#![cfg(windows)]

use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use windows_service::define_windows_service;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_dispatcher;

use crate::{run, AgentConfig, SystemActions, Target};

/// SCM short name (`sc query`).
pub const SERVICE_NAME: &str = "TernvaleGuestAgent";
/// Services MMC display name.
pub const SERVICE_DISPLAY: &str = "Ternvale Guest Agent";

define_windows_service!(ffi_service_main, service_main);

fn service_main(_args: Vec<OsString>) {
    if let Err(error) = run_service() {
        tracing::error!(target: "ternvale::agent", error = %error, "windows service failed");
    }
}

/// Hand the process to the Service Control Manager.
#[tracing::instrument(level = "info", target = "ternvale::agent", skip_all)]
pub fn dispatch() -> Result<(), windows_service::Error> {
    tracing::info!(target: "ternvale::agent", name = SERVICE_NAME, "starting as Windows service");
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
fn run_service() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let handle = service_control_handler::register(SERVICE_NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            flag.store(true, Ordering::Release);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::ZERO,
        process_id: None,
    })?;
    let mut actions = SystemActions::default();
    let config = AgentConfig::new(Target::auto_default());
    let exit = run(&config, &mut actions, &stop);
    tracing::info!(target: "ternvale::agent", ?exit, "windows service loop ended");
    handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::ZERO,
        process_id: None,
    })?;
    Ok(())
}

/// Register the service: auto start, restart on failure (5 s, three times).
#[tracing::instrument(level = "info", target = "ternvale::agent", skip_all)]
pub fn install() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let bin = format!("\"{}\" --service", exe.display());
    sc(&[
        "create",
        SERVICE_NAME,
        "binPath=",
        &bin,
        "start=",
        "auto",
        "DisplayName=",
        SERVICE_DISPLAY,
    ])?;
    sc(&[
        "failure",
        SERVICE_NAME,
        "reset=",
        "86400",
        "actions=",
        "restart/5000/restart/5000/restart/5000",
    ])?;
    sc(&["failureflag", SERVICE_NAME, "1"])?;
    tracing::info!(
        target: "ternvale::agent",
        exe = %exe.display(),
        name = SERVICE_NAME,
        "installed Windows service (auto start, restart on failure)"
    );
    Ok(())
}

fn sc(args: &[&str]) -> Result<(), String> {
    let status = std::process::Command::new("sc.exe")
        .args(args)
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("sc.exe {args:?} exited {status}"))
    }
}
