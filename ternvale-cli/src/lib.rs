//! `ternvale`: the command-line front end of the Ternvale VMM.
//!
//! `ternvale run <config>` boots a VM in the foreground and serves a Unix
//! control socket at `~/Library/Application Support/Ternvale/run/<name>.sock`
//! (JSON lines, see [`protocol`]). `status`, `pause`, `resume`, and `stop`
//! are clients of that socket. `serve` binds the JSON-RPC hub at
//! `run/api.sock` (`docs/API.md`). `validate` and `create-disk` work offline.
//! `doctor` checks the host, `logs` reads host logs, and `report` zips a
//! crash report (asking a running VM to `dump-diagnostics` first).
//! `acpi-dump` reads the ACPI tables back from a probe VM and can diff them
//! against QEMU's. `export` / `import` move `.tvm` directory bundles; `template
//! create` and `new --from-template` freeze a read-only base and linked-clone it.
//!
//! This is the only crate that uses `anyhow`; every error carries context
//! naming the operation. User-facing output uses `println!`/`eprintln!`;
//! everything else logs on `ternvale::cli`.

pub mod acpi;
pub mod acpi_fault;
pub mod api_hub;
pub mod api_library;
pub mod api_server;
pub mod balloon_cmd;
pub mod cli;
pub mod client;
pub mod clipboard;
pub mod clone_cmd;
pub mod commands;
pub mod control_agent;
pub mod cp_cmd;
pub mod create_cmd;
pub mod create_macos;
pub mod create_progress;
pub mod crypto_cmd;
pub mod diag;
pub mod disk;
pub mod disk_cmd;
pub mod display;
pub mod display_input;
pub mod doctor;
pub mod events_sub;
pub mod exec_cmd;
pub mod export_cmd;
pub mod fex_cmd;
pub mod guest_setup;
pub mod import_cmd;
pub mod json_out;
pub mod keychain;
pub mod logfmt;
pub mod logs;
pub mod macos_clone;
pub mod paths;
pub mod peer;
pub mod power;
pub mod protocol;
pub mod qemu_img;
pub mod release_verify;
pub mod report;
pub mod rpc_vm;
pub mod run;
pub mod sandbox_run;
pub mod seamless;
pub mod secret;
pub mod server;
pub mod snapshot_cmd;
pub mod ssh_cmd;
pub mod stats_cmd;
pub mod summary;
pub mod template_cmd;
pub mod tools;
pub mod tools_build;
pub mod tools_hybrid;
pub mod tools_manifest;
pub mod tools_verify;
pub mod tools_winfsp;
pub mod tvm;
pub mod tvm_io;
pub mod unattend_iso;
pub mod unattend_xml;
pub mod update_check;
pub mod vsock;
pub mod wait_cmd;

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
    match dispatch(cli) {
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
pub fn dispatch(cli: Cli) -> Result<ExitCode> {
    let json = cli.json;
    match cli.command {
        Command::Run {
            config,
            display,
            fps,
            headless,
        } => {
            let mode = if headless {
                crate::display::DisplayMode::None
            } else {
                display
            };
            run::run(&config, mode, fps)
        }
        Command::Doctor {
            config,
            install_rosetta,
        } => {
            // The log directory is one of the things doctor checks, so a
            // broken one must not stop it.
            let _guard = match cli_log() {
                Ok(guard) => Some(guard),
                Err(error) => {
                    eprintln!("warning: no log file: {}", error_chain(&error));
                    None
                }
            };
            doctor::doctor(json, config.as_deref(), install_rosetta)
        }
        other => {
            let _guard = cli_log()?;
            tracing::debug!(target: "ternvale::cli", command = ?other, "dispatch");
            non_run(other, json)
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

fn non_run(command: Command, json: bool) -> Result<ExitCode> {
    match command {
        Command::Serve => {
            let server = crate::api_server::ApiServer::start()?;
            json_out::emit(
                json,
                serde_json::json!({"ok": true, "socket": server.path().display().to_string()}),
                || println!("listening {}", server.path().display()),
            )?;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
        Command::Run {
            config,
            display,
            fps,
            headless,
        } => {
            let mode = if headless {
                crate::display::DisplayMode::None
            } else {
                display
            };
            run::run(&config, mode, fps)
        }
        Command::Validate { config } => commands::validate(&config, json),
        Command::Exec {
            name,
            timeout,
            argv,
        } => exec_cmd::exec(&name, timeout, &argv, json),
        Command::Cp { src, dst } => cp_cmd::cp(&src, &dst, json),
        Command::GuestSetup {
            name,
            dry_run,
            undo,
        } => guest_setup::guest_setup(&name, dry_run, undo, json),
        Command::X86 { action } => match action {
            cli::X86Command::FetchRootfs { distro, dest } => {
                fex_cmd::fetch_rootfs(&distro, dest.as_deref(), json)
            }
        },
        Command::WaitReady { name, timeout } => wait_cmd::wait_ready(&name, timeout, json),
        Command::Ssh { name } => ssh_cmd::ssh(&name, json),
        Command::Create {
            name,
            windows,
            macos,
            ipsw,
            sha256,
            iso,
            unattended,
            edition,
            disk_size,
            cpus,
            ram_mib,
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
            if macos {
                if windows {
                    anyhow::bail!("choose one of --windows or --macos");
                }
                return create_macos::create_macos(create_macos::CreateMacos {
                    name,
                    ipsw,
                    sha256,
                    disk_size,
                    cpus,
                    ram_mib,
                    wait,
                });
            }
            if !windows {
                anyhow::bail!("`ternvale create` requires --windows or --macos");
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
        Command::CreateDisk { path, size } => commands::create_disk(&path, &size, json),
        Command::Disk { action } => match action {
            cli::DiskCommand::Compact { vm, disk } => disk_cmd::compact(&vm, disk, json),
            cli::DiskCommand::Resize {
                vm,
                size,
                force,
                disk,
            } => disk_cmd::resize(&vm, &size, force, disk, json),
        },
        Command::Status { name, stats } => commands::status(&name, stats, json),
        Command::Stats {
            name,
            watch,
            interval_ms,
        } => stats_cmd::stats(&name, watch, interval_ms, json),
        Command::Balloon {
            name,
            target_mib,
            auto,
        } => balloon_cmd::balloon(&name, target_mib, auto, json),
        Command::Pause { name, timeout_ms } => commands::pause(&name, timeout_ms, json),
        Command::Resume { name } => commands::resume(&name, json),
        Command::Stop {
            name,
            force,
            graceful,
            no_wait,
        } => commands::stop(&name, force, graceful, no_wait, json),
        Command::Snapshot { action } => match action {
            cli::SnapshotCommand::Create { vm, name } => snapshot_cmd::create(&vm, &name, json),
            cli::SnapshotCommand::Restore { vm, name } => snapshot_cmd::restore(&vm, &name, json),
            cli::SnapshotCommand::List { vm } => snapshot_cmd::list(&vm, json),
            cli::SnapshotCommand::Delete { vm, name } => snapshot_cmd::delete(&vm, &name, json),
        },
        Command::Clone { source, new_name } => clone_cmd::clone_vm(&source, &new_name),
        Command::Export { vm, out, zstd } => export_cmd::export_vm(&vm, &out, zstd, json),
        Command::Import {
            source,
            name,
            layered,
            kernel,
            firmware,
        } => import_cmd::import(
            &source,
            name.as_deref(),
            layered,
            kernel.as_deref(),
            firmware.as_deref(),
            json,
        ),
        Command::Template { action } => match action {
            cli::TemplateCommand::Create { vm, name } => template_cmd::create(&vm, &name, json),
        },
        Command::New {
            name,
            from_template,
        } => template_cmd::new_from_template(&name, &from_template, json),
        Command::Crypto { action } => match action {
            cli::CryptoCommand::Rotate { name } => {
                crate::crypto_cmd::rotate(&name)?;
                Ok(ExitCode::SUCCESS)
            }
        },
        Command::Vm { action } => match action {
            cli::VmCommand::Delete { name } => clone_cmd::delete_vm(&name),
        },
        Command::Doctor {
            config,
            install_rosetta,
        } => doctor::doctor(json, config.as_deref(), install_rosetta),
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

#[cfg(test)]
#[path = "api_rpc_tests.rs"]
mod api_rpc_tests;
