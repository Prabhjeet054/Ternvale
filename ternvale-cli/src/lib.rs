//! `ternvale`: the command-line front end of the Ternvale VMM.
//!
//! `ternvale run <config>` boots a VM in the foreground and serves a Unix
//! control socket at `~/Library/Application Support/Ternvale/run/<name>.sock`
//! (JSON lines, see [`protocol`]). `status`, `pause`, `resume`, and `stop`
//! are clients of that socket. `validate` and `create-disk` work offline.
//!
//! This is the only crate that uses `anyhow`; every error carries context
//! naming the operation. User-facing output uses `println!`/`eprintln!`;
//! everything else logs on `ternvale::cli`.

pub mod cli;
pub mod client;
pub mod commands;
pub mod disk;
pub mod paths;
pub mod protocol;
pub mod run;
pub mod server;

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
        Command::Run { config } => run::run(&config),
        other => {
            let mut log = LogConfig::new(
                CLI_LOG_NAME,
                LogConfig::default_log_dir().context("find the log directory")?,
            );
            log.level = "warn".to_string();
            let _guard = ternvale_log::init(log).context("initialize logging")?;
            tracing::debug!(target: "ternvale::cli", command = ?other, "dispatch");
            non_run(other)
        }
    }
}

fn non_run(command: Command) -> Result<ExitCode> {
    match command {
        Command::Run { config } => run::run(&config),
        Command::Validate { config } => commands::validate(&config),
        Command::CreateDisk { path, size } => commands::create_disk(&path, &size),
        Command::Status { name, stats, json } => commands::status(&name, stats, json),
        Command::Pause {
            name,
            timeout_ms,
            json,
        } => commands::pause(&name, timeout_ms, json),
        Command::Resume { name, json } => commands::resume(&name, json),
        Command::Stop {
            name,
            force,
            no_wait,
            json,
        } => commands::stop(&name, force, no_wait, json),
    }
}

#[cfg(test)]
mod tests;
