//! Gateway listen policy and limits.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::GatewayError;

/// Default HTTPS listen address (loopback only).
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8443";
/// Default JSON body cap (matches hub JSON-RPC line limit).
pub const DEFAULT_MAX_BODY: usize = 64 * 1024;
/// Default per-token requests in a rolling minute.
pub const DEFAULT_RATE_PER_MINUTE: u32 = 60;

/// TOML file loaded by `ternvaled`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayFile {
    /// `host:port`. Default [`DEFAULT_LISTEN`].
    #[serde(default)]
    pub listen: Option<String>,
    /// Must be `true` to bind a non-loopback address.
    #[serde(default)]
    pub allow_remote_bind: bool,
    /// Reject clients that do not present a certificate signed by the gateway CA.
    #[serde(default)]
    pub require_mtls: bool,
    /// JSON body limit in bytes.
    #[serde(default)]
    pub max_body_bytes: Option<usize>,
    /// Rolling-window rate limit per token id.
    #[serde(default)]
    pub rate_limit_per_minute: Option<u32>,
    /// Directory for CA, tokens (`TVTK`), and the audit log.
    #[serde(default)]
    pub state_dir: Option<PathBuf>,
    /// Hub Unix socket (`api.sock`). Default: run dir.
    #[serde(default)]
    pub hub_socket: Option<PathBuf>,
    /// Framebuffer dump directory (`<name>.fb`). Default: run dir.
    #[serde(default)]
    pub run_dir: Option<PathBuf>,
    /// Library VM configs (`<name>/config.toml`) for serial tails.
    #[serde(default)]
    pub vms_dir: Option<PathBuf>,
}

/// Resolved runtime config.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Bind address after policy checks.
    pub listen: SocketAddr,
    /// Non-loopback bind was explicitly allowed.
    pub allow_remote_bind: bool,
    /// Client certificates required.
    pub require_mtls: bool,
    /// Max HTTP body.
    pub max_body_bytes: usize,
    /// Requests per token per minute.
    pub rate_limit_per_minute: u32,
    /// State directory.
    pub state_dir: PathBuf,
    /// Control hub socket.
    pub hub_socket: PathBuf,
    /// Framebuffer dumps.
    pub run_dir: PathBuf,
    /// VM library (serial_log paths).
    pub vms_dir: PathBuf,
}

impl GatewayConfig {
    /// Built-in defaults (loopback, user Application Support).
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
    pub fn default_values() -> Result<Self, GatewayError> {
        Self::from_file(&GatewayFile {
            listen: None,
            allow_remote_bind: false,
            require_mtls: false,
            max_body_bytes: None,
            rate_limit_per_minute: None,
            state_dir: None,
            hub_socket: None,
            run_dir: None,
            vms_dir: None,
        })
    }

    /// Load TOML from `path`.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(path = %path.display()))]
    pub fn load(path: &Path) -> Result<Self, GatewayError> {
        let text = std::fs::read_to_string(path).map_err(|source| {
            GatewayError::io_path("read gateway config", path.display(), source)
        })?;
        let file: GatewayFile = toml::from_str(&text)?;
        Self::from_file(&file)
    }

    /// Resolve `file` and enforce the loopback bind policy.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
    pub fn from_file(file: &GatewayFile) -> Result<Self, GatewayError> {
        let listen: SocketAddr = file
            .listen
            .as_deref()
            .unwrap_or(DEFAULT_LISTEN)
            .parse()
            .map_err(|error| GatewayError::Config(format!("listen address: {error}")))?;
        check_bind(listen, file.allow_remote_bind)?;
        let state_dir = match &file.state_dir {
            Some(path) => path.clone(),
            None => default_state_dir()?,
        };
        let hub_socket = match &file.hub_socket {
            Some(path) => path.clone(),
            None => default_hub_socket()?,
        };
        let run_dir = file
            .run_dir
            .clone()
            .unwrap_or_else(crate::extras::default_run_dir);
        let vms_dir = file
            .vms_dir
            .clone()
            .unwrap_or_else(crate::extras::default_vms_dir);
        let max_body_bytes = file.max_body_bytes.unwrap_or(DEFAULT_MAX_BODY);
        if !(1024..=1024 * 1024).contains(&max_body_bytes) {
            return Err(GatewayError::Config(
                "max_body_bytes must be between 1024 and 1048576".into(),
            ));
        }
        let rate_limit_per_minute = file
            .rate_limit_per_minute
            .unwrap_or(DEFAULT_RATE_PER_MINUTE);
        if rate_limit_per_minute == 0 {
            return Err(GatewayError::Config(
                "rate_limit_per_minute must be at least 1".into(),
            ));
        }
        tracing::info!(
            target: "ternvale::gateway",
            listen = %listen,
            allow_remote_bind = file.allow_remote_bind,
            require_mtls = file.require_mtls,
            max_body_bytes,
            rate_limit_per_minute,
            state_dir = %state_dir.display(),
            hub_socket = %hub_socket.display(),
            run_dir = %run_dir.display(),
            vms_dir = %vms_dir.display(),
            "gateway config resolved"
        );
        Ok(Self {
            listen,
            allow_remote_bind: file.allow_remote_bind,
            require_mtls: file.require_mtls,
            max_body_bytes,
            rate_limit_per_minute,
            state_dir,
            hub_socket,
            run_dir,
            vms_dir,
        })
    }
}

/// Refuse a non-loopback bind unless `allow_remote_bind` is set.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(listen = %listen, allow_remote_bind))]
pub fn check_bind(listen: SocketAddr, allow_remote_bind: bool) -> Result<(), GatewayError> {
    if is_loopback(listen.ip()) {
        return Ok(());
    }
    if allow_remote_bind {
        tracing::warn!(
            target: "ternvale::gateway",
            listen = %listen,
            "binding a non-loopback address; allow_remote_bind is set"
        );
        return Ok(());
    }
    Err(GatewayError::Config(format!(
        "refusing to bind {listen}: set allow_remote_bind = true to listen on a network interface"
    )))
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn default_state_dir() -> Result<PathBuf, GatewayError> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        GatewayError::Config("HOME is not set; cannot find the gateway state directory".into())
    })?;
    Ok(PathBuf::from(home).join("Library/Application Support/Ternvale/gateway"))
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn default_hub_socket() -> Result<PathBuf, GatewayError> {
    if let Some(dir) = std::env::var_os("TERNVALE_RUN_DIR") {
        return Ok(PathBuf::from(dir).join("api.sock"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        GatewayError::Config("HOME is not set; cannot find the hub socket".into())
    })?;
    Ok(PathBuf::from(home).join("Library/Application Support/Ternvale/run/api.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_allowed_without_flag() {
        check_bind("127.0.0.1:8443".parse().expect("addr"), false).expect("loopback");
    }

    #[test]
    fn lan_bind_requires_explicit_flag() {
        let err = check_bind("0.0.0.0:8443".parse().expect("addr"), false).expect_err("lan");
        assert!(err.to_string().contains("allow_remote_bind"));
        check_bind("0.0.0.0:8443".parse().expect("addr"), true).expect("flag");
    }
}
