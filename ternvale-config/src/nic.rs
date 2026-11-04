//! Per-NIC backend and vmnet operating mode.

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;

/// vmnet operating mode (`<vmnet/vmnet.h>` `operating_modes_t` names).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NicMode {
    /// NAT onto the host network (`VMNET_SHARED_MODE`). Default.
    #[default]
    Shared,
    /// Isolated LAN between VMs and the host; no Internet (`VMNET_HOST_MODE`).
    HostOnly,
    /// L2 bridge onto a physical NIC (`VMNET_BRIDGED_MODE`).
    Bridged,
}

impl NicMode {
    pub(crate) fn is_shared(&self) -> bool {
        matches!(self, Self::Shared)
    }

    /// Wire / log name.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::HostOnly => "host-only",
            Self::Bridged => "bridged",
        }
    }
}

/// One virtual NIC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nic {
    /// Host backend name (`loopback` or `vmnet`).
    pub backend: String,
    /// vmnet operating mode. Ignored for `loopback`. Default `shared`.
    #[serde(default, skip_serializing_if = "NicMode::is_shared")]
    pub mode: NicMode,
    /// Host-only virtual network name. VMs that share this name join one LAN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    /// Physical interface for `mode = "bridged"` (for example `en0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_interface: Option<String>,
}

impl Nic {
    /// Default loopback NIC.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn loopback() -> Self {
        Self {
            backend: "loopback".into(),
            mode: NicMode::Shared,
            network: None,
            bridge_interface: None,
        }
    }

    /// Host-only LAN name, or `"default"` when unset.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn host_network(&self) -> &str {
        self.network
            .as_deref()
            .filter(|n| !n.is_empty())
            .unwrap_or("default")
    }

    /// Check backend/mode pairing. `index` is the `nics` slot.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(index, backend = %self.backend))]
    pub fn validate(&self, index: usize) -> Result<(), ConfigError> {
        if self.backend.is_empty() {
            tracing::error!(target: "ternvale::config", index, "rejected NIC with empty backend");
            return Err(ConfigError::EmptyNicBackend { index });
        }
        match self.backend.as_str() {
            "loopback" => self.validate_loopback(index),
            "vmnet" => self.validate_vmnet(index),
            other => {
                tracing::debug!(
                    target: "ternvale::config",
                    index,
                    backend = other,
                    "NIC backend left to the net device"
                );
                Ok(())
            }
        }
    }

    fn validate_loopback(&self, index: usize) -> Result<(), ConfigError> {
        if !self.mode.is_shared() {
            return Err(nic_err(
                index,
                "mode",
                "loopback has no vmnet mode; omit mode or use backend = \"vmnet\"",
            ));
        }
        if self.network.is_some() || self.bridge_interface.is_some() {
            return Err(nic_err(
                index,
                "network",
                "network and bridge_interface apply only to backend = \"vmnet\"",
            ));
        }
        Ok(())
    }

    fn validate_vmnet(&self, index: usize) -> Result<(), ConfigError> {
        match self.mode {
            NicMode::Shared => {
                if self.bridge_interface.is_some() {
                    return Err(nic_err(
                        index,
                        "bridge_interface",
                        "bridge_interface is only valid with mode = \"bridged\"",
                    ));
                }
            }
            NicMode::HostOnly => {
                if self.bridge_interface.is_some() {
                    return Err(nic_err(
                        index,
                        "bridge_interface",
                        "bridge_interface is only valid with mode = \"bridged\"",
                    ));
                }
                if let Some(name) = &self.network {
                    validate_ident(index, "network", name)?;
                }
            }
            NicMode::Bridged => {
                let Some(iface) = self.bridge_interface.as_deref() else {
                    return Err(nic_err(
                        index,
                        "bridge_interface",
                        "mode = \"bridged\" needs bridge_interface (for example \"en0\")",
                    ));
                };
                validate_iface(index, iface)?;
                if self.network.is_some() {
                    return Err(nic_err(
                        index,
                        "network",
                        "network is only valid with mode = \"host-only\"",
                    ));
                }
            }
        }
        tracing::debug!(
            target: "ternvale::config",
            index,
            backend = %self.backend,
            mode = self.mode.as_str(),
            network = self.network.as_deref().unwrap_or(""),
            bridge = self.bridge_interface.as_deref().unwrap_or(""),
            "accepted NIC"
        );
        Ok(())
    }
}

fn nic_err(index: usize, field: &'static str, reason: &'static str) -> ConfigError {
    tracing::error!(target: "ternvale::config", index, field, reason, "rejected NIC");
    ConfigError::InvalidNic {
        index,
        field,
        reason: reason.into(),
    }
}

fn validate_ident(index: usize, field: &'static str, value: &str) -> Result<(), ConfigError> {
    if value.is_empty() || value.len() > 32 {
        return Err(ConfigError::InvalidNic {
            index,
            field,
            reason: "must be 1..=32 ASCII letters, digits, '-' or '_'".into(),
        });
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ConfigError::InvalidNic {
            index,
            field,
            reason: "must be 1..=32 ASCII letters, digits, '-' or '_'".into(),
        });
    }
    Ok(())
}

fn validate_iface(index: usize, value: &str) -> Result<(), ConfigError> {
    if value.is_empty() || value.len() > 15 {
        return Err(ConfigError::InvalidNic {
            index,
            field: "bridge_interface",
            reason: "must be a BSD interface name of at most 15 bytes (for example en0)".into(),
        });
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ConfigError::InvalidNic {
            index,
            field: "bridge_interface",
            reason: "must be a BSD interface name (letters, digits, '-' or '_')".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Nic, NicMode};

    fn vmnet(mode: NicMode) -> Nic {
        Nic {
            backend: "vmnet".into(),
            mode,
            network: None,
            bridge_interface: None,
        }
    }

    #[test]
    fn host_only_defaults_network_name() {
        let nic = vmnet(NicMode::HostOnly);
        assert_eq!(nic.host_network(), "default");
        nic.validate(0).expect("host-only");
    }

    #[test]
    fn bridged_needs_interface() {
        let err = vmnet(NicMode::Bridged).validate(1).expect_err("no iface");
        assert!(err.to_string().contains("bridge_interface"), "{err}");
        let mut nic = vmnet(NicMode::Bridged);
        nic.bridge_interface = Some("en0".into());
        nic.validate(1).expect("en0");
    }

    #[test]
    fn loopback_rejects_vmnet_fields() {
        let mut nic = Nic::loopback();
        nic.mode = NicMode::HostOnly;
        assert!(nic.validate(0).is_err());
    }
}
