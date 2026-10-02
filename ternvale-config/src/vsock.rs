//! `[vsock]` section: a virtio-vsock device and the guest agent server.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;

/// Longest `uds_dir` that still fits `guest-<u32>.sock` in the 104-byte
/// macOS `sun_path` (including the NUL).
pub const MAX_UDS_DIR: usize = 103 - "/guest-4294967295.sock".len();

/// virtio-vsock device settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VsockSection {
    /// Guest CID (3 or higher). Omitted: the lowest free CID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cid: Option<u32>,
    /// Directory for the per-port Unix sockets. Omitted: `<name>.vsock` in
    /// the Ternvale run directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uds_dir: Option<PathBuf>,
    /// Run the guest agent server on port 5000. Default true.
    #[serde(default = "yes")]
    pub agent: bool,
    /// Guest ports host processes may connect to via `guest-<port>.sock`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub listen_ports: Vec<u32>,
}

impl Default for VsockSection {
    fn default() -> Self {
        Self {
            cid: None,
            uds_dir: None,
            agent: true,
            listen_ports: Vec::new(),
        }
    }
}

fn yes() -> bool {
    true
}

impl VsockSection {
    /// Range checks: CID not reserved, `uds_dir` short enough, ports valid.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(cid = ?self.cid, agent = self.agent))]
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |field: &'static str, reason: String| {
            tracing::error!(target: "ternvale::config", field, reason = %reason, "rejected vsock setting");
            ConfigError::InvalidVsock { field, reason }
        };
        if let Some(cid) = self.cid {
            if !(3..u32::MAX).contains(&cid) {
                return Err(invalid(
                    "vsock.cid",
                    format!("{cid} is reserved; use 3 through {}", u32::MAX - 1),
                ));
            }
        }
        if let Some(dir) = &self.uds_dir {
            let len = dir.as_os_str().len();
            if len == 0 || len > MAX_UDS_DIR {
                return Err(invalid(
                    "vsock.uds_dir",
                    format!("must be 1..={MAX_UDS_DIR} bytes (macOS socket path limit), got {len}"),
                ));
            }
        }
        for (index, &port) in self.listen_ports.iter().enumerate() {
            if port == 0 || port == u32::MAX || self.listen_ports[..index].contains(&port) {
                return Err(invalid(
                    "vsock.listen_ports",
                    format!("port {port} is zero, reserved, or listed twice"),
                ));
            }
        }
        tracing::debug!(target: "ternvale::config", cid = ?self.cid, uds_dir = ?self.uds_dir, agent = self.agent, ports = ?self.listen_ports, "accepted vsock section");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_enable_the_agent_and_pick_a_cid() {
        let section: VsockSection = toml::from_str("").expect("empty");
        assert_eq!(section, VsockSection::default());
        assert!(section.agent);
        section.validate().expect("valid");
    }

    #[test]
    fn rejects_reserved_cids_long_dirs_and_bad_ports() {
        for cid in [0, 1, 2, u32::MAX] {
            let section = VsockSection {
                cid: Some(cid),
                ..VsockSection::default()
            };
            let error = section.validate().expect_err("reserved");
            assert!(error.to_string().starts_with("vsock.cid:"), "{error}");
        }
        let section = VsockSection {
            uds_dir: Some(PathBuf::from("/".repeat(MAX_UDS_DIR + 1))),
            ..VsockSection::default()
        };
        assert!(section.validate().is_err());
        let section = VsockSection {
            uds_dir: Some(PathBuf::from("x".repeat(MAX_UDS_DIR))),
            listen_ports: vec![22, 22],
            ..VsockSection::default()
        };
        let error = section.validate().expect_err("duplicate");
        assert!(error.to_string().contains("listed twice"), "{error}");
    }

    #[test]
    fn rejects_unknown_keys() {
        let error = toml::from_str::<VsockSection>("port = 1").expect_err("unknown");
        assert!(error.to_string().contains("port"), "{error}");
    }
}
