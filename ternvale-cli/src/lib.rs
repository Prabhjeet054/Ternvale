//! `ternvale`: the command-line front end of the Ternvale VMM.
//!
//! `ternvale run <config>` boots a VM in the foreground and serves a Unix
//! control socket at `~/Library/Application Support/Ternvale/run/<name>.sock`
//! (JSON lines, see [`protocol`]). `status`, `pause`, `resume`, and `stop`
//! are clients of that socket. `validate` and `create-disk` work offline.
//! `doctor` checks the host, `logs` reads host logs, and `report` zips a
//! crash report (asking a running VM to `dump-diagnostics` first).
//! `acpi-dump` reads the ACPI tables back from a probe VM and can diff them
//! against QEMU's.
//!
//! This is the only crate that uses `anyhow`; every error carries context
//! naming the operation. User-facing output uses `println!`/`eprintln!`;
//! everything else logs on `ternvale::cli`.

pub mod acpi;
pub mod acpi_fault;
pub mod balloon_cmd;
pub mod cli;
pub mod client;
pub mod clipboard;
pub mod clone_cmd;
pub mod commands;
pub mod create_cmd;
pub mod create_progress;
pub mod diag;
pub mod disk;
pub mod display;
pub mod display_input;
pub mod doctor;
pub mod keychain;
pub mod logfmt;
pub mod logs;
pub mod paths;
pub mod power;
pub mod protocol;
pub mod release_verify;
pub mod report;
pub mod run;
pub mod seamless;
pub mod secret;
pub mod server;
pub mod snapshot_cmd;
pub mod stats_cmd;
pub mod summary;
pub mod tools;
pub mod tools_build;
pub mod tools_hybrid;
pub mod tools_manifest;
pub mod tools_verify;
pub mod tools_winfsp;
pub mod unattend_iso;
pub mod unattend_xml;
pub mod update_check;
pub mod vsock;

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use ternvale_log::LogConfig;

use crate::cli::{Cli, Command};

/// Log-file name for every command except `run`, which uses the VM name.
pub const CLI_LOG_NAME: &str = "cli";

/// Parse arguments, run the command, and turn errors into exit status 1.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli.command) {
        Ok(code) => code,
        Err(error) => {
            let message = error_chain(&error);
            tracing::error!(target: "ternvale::cli", error = %message, "command failed");
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// `context: cause: cause`, skipping a cause whose text the message already
/// ends with (library errors often embed their source in their own text).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn error_chain(error: &anyhow::Error) -> String {
    let mut message = String::new();
    for cause in error.chain() {
        let text = cause.to_string();
        if message.is_empty() {
            message = text;
        } else if !message.ends_with(&text) {
            message = format!("{message}: {text}");
        }
    }
    message
}

/// Run one subcommand. `run` sets up its own per-VM logging once the
/// config names the VM; the rest log at `warn` (or `TERNVALE_LOG`) to the
/// `cli` log file.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn dispatch(command: Command) -> Result<ExitCode> {
    match command {
        Command::Run {
            config,
            display,
            fps,
        } => run::run(&config, display, fps),
        Command::Doctor { json, config } => {
            // The log directory is one of the things doctor checks, so a
            // broken one must not stop it.
            let _guard = match cli_log() {
                Ok(guard) => Some(guard),
                Err(error) => {
                    eprintln!("warning: no log file: {}", error_chain(&error));
                    None
                }
            };
            doctor::doctor(json, config.as_deref())
        }
        other => {
            let _guard = cli_log()?;
            tracing::debug!(target: "ternvale::cli", command = ?other, "dispatch");
            non_run(other)
        }
    }
}

fn cli_log() -> Result<ternvale_log::LogGuard> {
    let mut log = LogConfig::new(
        CLI_LOG_NAME,
        LogConfig::default_log_dir().context("find the log directory")?,
    );
    log.level = "warn".to_string();
    ternvale_log::init(log).context("initialize logging")
}

fn non_run(command: Command) -> Result<ExitCode> {
    match command {
        Command::Run {
            config,
            display,
            fps,
        } => run::run(&config, display, fps),
        Command::Validate { config } => commands::validate(&config),
        Command::Create {
            name,
            windows,
            iso,
            unattended,
            edition,
            disk_size,
            firmware,
            tools_iso,
            user,
            password_env,
            password_file,
            product_key_env,
            product_key_file,
            wait,
            timeout_secs,
        } => {
            if !windows {
                anyhow::bail!("`ternvale create` currently supports --windows only");
            }
            let iso = iso.ok_or_else(|| {
                anyhow::anyhow!("--iso PATH is required (your Windows 11 ARM64 installer)")
            })?;
            create_cmd::create_windows(create_cmd::CreateWindows {
                name,
                iso,
                unattended,
                edition,
                disk_size,
                firmware,
                tools_iso,
                user,
                password_env,
                password_file,
                product_key_env,
                product_key_file,
                wait,
                timeout_secs,
            })
        }
        Command::CreateDisk { path, size } => commands::create_disk(&path, &size),
        Command::Status { name, stats, json } => commands::status(&name, stats, json),
        Command::Stats {
            name,
            watch,
            interval_ms,
            json,
        } => stats_cmd::stats(&name, watch, interval_ms, json),
        Command::Balloon {
            name,
            target_mib,
            auto,
            json,
        } => balloon_cmd::balloon(&name, target_mib, auto, json),
        Command::Pause {
            name,
            timeout_ms,
            json,
        } => commands::pause(&name, timeout_ms, json),
        Command::Resume { name, json } => commands::resume(&name, json),
        Command::Stop {
            name,
            force,
            graceful,
            no_wait,
            json,
        } => commands::stop(&name, force, graceful, no_wait, json),
        Command::Snapshot { action } => match action {
            cli::SnapshotCommand::Create { vm, name, json } => {
                snapshot_cmd::create(&vm, &name, json)
            }
            cli::SnapshotCommand::Restore { vm, name, json } => {
                snapshot_cmd::restore(&vm, &name, json)
            }
            cli::SnapshotCommand::List { vm, json } => snapshot_cmd::list(&vm, json),
            cli::SnapshotCommand::Delete { vm, name, json } => {
                snapshot_cmd::delete(&vm, &name, json)
            }
        },
        Command::Clone { source, new_name } => clone_cmd::clone_vm(&source, &new_name),
        Command::Vm { action } => match action {
            cli::VmCommand::Delete { name } => clone_cmd::delete_vm(&name),
        },
        Command::Doctor { json, config } => doctor::doctor(json, config.as_deref()),
        Command::Logs {
            name,
            follow,
            level,
            targets,
            list,
            file,
        } => {
            logs::check_targets(&targets)?;
            logs::logs(&logs::LogsArgs {
                name,
                follow,
                filter: logfmt::Filter { level, targets },
                list,
                file,
            })
        }
        Command::Report { name, out, config } => {
            report::report(&name, out.as_deref(), config.as_deref())
        }
        Command::AcpiDump {
            out,
            qemu_ram,
            ram_base,
            cpus,
            offline,
        } => acpi::acpi_dump(&out, qemu_ram.as_deref(), ram_base, cpus, offline),
        Command::UpdateCheck { feed } => update_check::update_check(&feed),
        Command::Tools { action } => tools::tools(action),
        Command::ReleaseVerify {
            app,
            expect_gatekeeper,
        } => release_verify::release_verify(&app, expect_gatekeeper),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "agent_tests.rs"]
mod agent_tests;

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tools_tests;

#[cfg(test)]
#[path = "create_tests.rs"]
mod create_tests;
