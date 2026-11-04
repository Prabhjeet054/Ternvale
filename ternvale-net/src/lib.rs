//! Frame protocol between the unprivileged VMM and `ternvale-vmnetd`.

mod client;
mod helper;
mod protocol;

pub use client::Client;
pub use helper::{read_open, serve, serve_client, Port, PortInfo};
pub use protocol::{Hello, Msg, Open, MSG_DATA, MSG_DROP, MSG_LINK};

use std::path::{Path, PathBuf};

/// Env override for the helper socket.
pub const SOCK_ENV: &str = "TERNVALE_VMNET_SOCK";
/// launchd default.
pub const SYSTEM_SOCK: &str = "/var/run/ternvale-vmnetd.sock";

/// Socket the VMM should connect to.
#[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
pub fn socket_path() -> PathBuf {
    if let Ok(path) = std::env::var(SOCK_ENV) {
        return PathBuf::from(path);
    }
    if Path::new(SYSTEM_SOCK).exists() {
        return PathBuf::from(SYSTEM_SOCK);
    }
    dirs_run().join("vmnet.sock")
}

fn dirs_run() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join("Library/Application Support/Ternvale/run")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-net");
    }
}
