//! virtio-net backend that talks to `ternvale-vmnetd` (shared/NAT vmnet).
//!
//! The VMM process never calls vmnet.framework. Built with `--features vmnet`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::backend::NetBackend;
use super::NetError;
use ternvale_net::{socket_path, Client};

/// `operating_modes_t` value for NAT shared with the host (`<vmnet/vmnet.h>`).
pub const VMNET_SHARED_MODE: u32 = 1001;

const RECONNECT: Duration = Duration::from_millis(200);

/// Unprivileged client of the privileged vmnet helper.
pub struct VmnetBackend {
    path: PathBuf,
    client: Option<Client>,
    retry_at: Option<Instant>,
}

impl VmnetBackend {
    /// Connect to the helper socket (`TERNVALE_VMNET_SOCK` or the launchd path).
    #[tracing::instrument(level = "debug", target = "ternvale::net::vmnet")]
    pub fn open_shared() -> Result<Self, NetError> {
        let path = socket_path();
        tracing::info!(
            target: "ternvale::net::vmnet",
            path = %path.display(),
            mode = VMNET_SHARED_MODE,
            "opening vmnet helper client"
        );
        let client = Client::connect(&path).map_err(|error| {
            tracing::error!(
                target: "ternvale::net::vmnet",
                path = %path.display(),
                error = %error,
                "vmnet helper unavailable"
            );
            NetError::Vmnet {
                reason: format!("{}: {error}", path.display()),
            }
        })?;
        Ok(Self {
            path,
            client: Some(client),
            retry_at: None,
        })
    }

    fn drop_client(&mut self, reason: &str) {
        if self.client.take().is_some() {
            tracing::info!(
                target: "ternvale::net::vmnet",
                path = %self.path.display(),
                reason,
                up = false,
                "vmnet helper link down"
            );
        }
        self.retry_at = Some(Instant::now() + RECONNECT);
    }

    fn ensure_client(&mut self) -> bool {
        if self.client.is_some() {
            return true;
        }
        if self.retry_at.is_some_and(|at| Instant::now() < at) {
            return false;
        }
        match Client::connect(&self.path) {
            Ok(client) => {
                tracing::info!(
                    target: "ternvale::net::vmnet",
                    path = %self.path.display(),
                    up = true,
                    "vmnet helper reconnected"
                );
                self.client = Some(client);
                self.retry_at = None;
                true
            }
            Err(error) => {
                tracing::debug!(
                    target: "ternvale::net::vmnet",
                    path = %self.path.display(),
                    error = %error,
                    "vmnet helper reconnect deferred"
                );
                self.retry_at = Some(Instant::now() + RECONNECT);
                false
            }
        }
    }
}

impl NetBackend for VmnetBackend {
    fn name(&self) -> &'static str {
        "vmnet"
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), NetError> {
        if !self.ensure_client() {
            tracing::warn!(
                target: "ternvale::net::vmnet",
                len = frame.len(),
                reason = "helper down",
                "packet drop"
            );
            return Err(NetError::Vmnet {
                reason: "helper down".into(),
            });
        }
        let Some(client) = self.client.as_mut() else {
            return Err(NetError::Vmnet {
                reason: "helper down".into(),
            });
        };
        match client.send_frame(frame) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.drop_client(&error.to_string());
                Err(NetError::Vmnet {
                    reason: error.to_string(),
                })
            }
        }
    }

    fn recv(&mut self) -> Result<Option<Vec<u8>>, NetError> {
        if !self.ensure_client() {
            return Ok(None);
        }
        let Some(client) = self.client.as_mut() else {
            return Ok(None);
        };
        match client.recv_frame() {
            Ok(frame) => Ok(frame),
            Err(error) => {
                self.drop_client(&error.to_string());
                Ok(None)
            }
        }
    }

    fn link_up(&self) -> bool {
        self.client.as_ref().is_some_and(Client::link_up)
    }

    fn guest_mac(&self) -> Option<[u8; 6]> {
        self.client.as_ref().map(|client| client.hello().mac)
    }
}

#[cfg(test)]
mod tests {
    use super::VMNET_SHARED_MODE;

    #[test]
    fn shared_mode_matches_sdk() {
        assert_eq!(VMNET_SHARED_MODE, 1001);
    }
}
