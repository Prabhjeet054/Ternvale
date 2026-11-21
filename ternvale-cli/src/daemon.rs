//! `ternvaled`: launchd-managed hub + HTTPS gateway.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ternvale_gateway::auth::{Scope, TokenStore};
use ternvale_gateway::certs::generate_pki;
use ternvale_gateway::config::GatewayConfig;
use ternvale_gateway::serve::{unix_hub, Gateway};
use ternvale_log::LogConfig;

/// Headless multi-VM daemon and HTTPS gateway.
#[derive(Debug, Parser)]
#[command(name = "ternvaled", version, about)]
struct DaemonCli {
    #[command(subcommand)]
    command: Option<DaemonCommand>,
    /// Gateway TOML (`listen`, `allow_remote_bind`, …).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Generate a CA, server cert, and sample client cert (overwrites).
    CertGenerate {
        /// Directory (default: gateway state dir).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Issue a hashed bearer token (secret printed once).
    TokenCreate {
        /// `read-only`, `operator`, or `admin`.
        #[arg(long)]
        scope: String,
        /// Optional unix expiry.
        #[arg(long)]
        expires_unix: Option<u64>,
    },
}

/// `ternvaled` entry.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            tracing::error!(target: "ternvale::gateway", error = %error, "ternvaled failed");
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = DaemonCli::parse();
    let config = match &cli.config {
        Some(path) => {
            GatewayConfig::load(path).with_context(|| format!("load {}", path.display()))?
        }
        None => GatewayConfig::default_values().context("default gateway config")?,
    };
    let mut log = LogConfig::new(
        "ternvaled",
        LogConfig::default_log_dir().context("find the log directory")?,
    );
    log.level = std::env::var("TERNVALE_LOG").unwrap_or_else(|_| "info".into());
    let _guard = ternvale_log::init(log).context("initialize logging")?;
    match cli.command {
        Some(DaemonCommand::CertGenerate { dir }) => {
            let dir = dir.unwrap_or(config.state_dir);
            generate_pki(&dir).context("generate certificates")?;
            println!("wrote CA and leaf certs in {}", dir.display());
            println!("rotation: see docs/GATEWAY.md");
            Ok(ExitCode::SUCCESS)
        }
        Some(DaemonCommand::TokenCreate {
            scope,
            expires_unix,
        }) => {
            let scope = Scope::parse(&scope).with_context(|| format!("unknown scope {scope:?}"))?;
            std::fs::create_dir_all(&config.state_dir)
                .with_context(|| format!("create {}", config.state_dir.display()))?;
            let path = config.state_dir.join("tokens.tvtk");
            let mut store = TokenStore::load(&path).context("load token store")?;
            let (id, secret) = store.issue(scope, expires_unix).context("issue token")?;
            println!("token_id={id}");
            println!("scope={}", scope.as_str());
            println!("{}", secret.expose());
            Ok(ExitCode::SUCCESS)
        }
        None => serve(config),
    }
}

fn serve(config: GatewayConfig) -> Result<ExitCode> {
    tracing::info!(
        target: "ternvale::gateway",
        listen = %config.listen,
        hub = %config.hub_socket.display(),
        "ternvaled starting"
    );
    let _hub = crate::api_server::ApiServer::start().context("start api.sock hub")?;
    let hub = unix_hub(&config);
    let _gateway = Gateway::bind(config, hub).context("start https gateway")?;
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
