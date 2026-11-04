//! Per-NIC outbound policy: LAN isolation, CIDR lists, rate limits, anti-spoof.

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::nic::ForwardProto;

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// One outbound match: destination CIDR, optional L4 port and protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CidrPort {
    /// IPv4 prefix, for example `10.0.0.0/8` or `1.1.1.1/32`.
    pub cidr: String,
    /// Destination port. Omit to match every port (and ICMP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// `tcp` or `udp`. Omit to match every protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proto: Option<ForwardProto>,
}

/// Outbound policy attached to one [`crate::Nic`].
///
/// Defaults keep existing VMs working: LAN is allowed, anti-spoof is on,
/// allow/deny lists are empty (permit), and rate limits are off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NicPolicy {
    /// When `false`, drop RFC1918 and IPv4 link-local destinations except the
    /// virtual gateway / DNS (user NAT) or a learned DHCP router (vmnet).
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub allow_lan: bool,
    /// If non-empty, only these destinations (CIDR + optional port/proto) pass.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<CidrPort>,
    /// Always dropped when the destination matches, before the allowlist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<CidrPort>,
    /// Token-bucket rate in bits per second. Omit for unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth_bps: Option<u64>,
    /// Token-bucket packets per second. Omit for unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pps: Option<u32>,
    /// Drop frames whose source MAC is not the virtio MAC, or whose source IP
    /// is not the leased address. DHCP from `0.0.0.0` is exempt.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub anti_spoof: bool,
}

impl Default for NicPolicy {
    fn default() -> Self {
        Self {
            allow_lan: true,
            allow: Vec::new(),
            deny: Vec::new(),
            bandwidth_bps: None,
            pps: None,
            anti_spoof: true,
        }
    }
}

impl NicPolicy {
    /// True when every field is the documented default.
    #[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// Parse CIDR strings and reject empty/zero rate limits.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(index))]
    pub fn validate(&self, index: usize) -> Result<(), ConfigError> {
        for (field, rules) in [("allow", &self.allow), ("deny", &self.deny)] {
            for (rule_i, rule) in rules.iter().enumerate() {
                parse_cidr(&rule.cidr).map_err(|reason| ConfigError::InvalidNic {
                    index,
                    field,
                    reason: format!("[{rule_i}] {reason}"),
                })?;
                if rule.port == Some(0) {
                    return Err(ConfigError::InvalidNic {
                        index,
                        field,
                        reason: format!("[{rule_i}] port 0 is invalid; omit port to match any"),
                    });
                }
            }
        }
        if self.bandwidth_bps == Some(0) {
            return Err(ConfigError::InvalidNic {
                index,
                field: "bandwidth_bps",
                reason: "must be at least 1, or omit the field".into(),
            });
        }
        if self.pps == Some(0) {
            return Err(ConfigError::InvalidNic {
                index,
                field: "pps",
                reason: "must be at least 1, or omit the field".into(),
            });
        }
        tracing::debug!(
            target: "ternvale::config",
            index,
            allow_lan = self.allow_lan,
            anti_spoof = self.anti_spoof,
            allow = self.allow.len(),
            deny = self.deny.len(),
            bandwidth_bps = self.bandwidth_bps.unwrap_or(0),
            pps = self.pps.unwrap_or(0),
            "accepted NIC policy"
        );
        Ok(())
    }
}

/// Parse `a.b.c.d/prefix` into (network, mask) in host order.
#[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
pub fn parse_cidr(text: &str) -> Result<(u32, u32), String> {
    let (addr, prefix) = text
        .split_once('/')
        .ok_or_else(|| format!("cidr {text:?} needs a /prefix"))?;
    let ip: std::net::Ipv4Addr = addr
        .parse()
        .map_err(|_| format!("cidr {text:?} is not an IPv4 address"))?;
    let bits: u32 = prefix
        .parse()
        .map_err(|_| format!("cidr {text:?} has a non-numeric prefix"))?;
    if bits > 32 {
        return Err(format!("cidr {text:?} prefix must be 0..=32"));
    }
    let mask = if bits == 0 {
        0
    } else {
        u32::MAX << (32 - bits)
    };
    let net = u32::from(ip) & mask;
    Ok((net, mask))
}

#[cfg(test)]
mod tests {
    use super::{parse_cidr, CidrPort};
    use crate::nic::Nic;

    #[test]
    fn default_policy_is_permissive_lan() {
        let nic: Nic = toml::from_str("backend = \"user\"").expect("parse");
        assert!(nic.policy.allow_lan);
        assert!(nic.policy.anti_spoof);
        assert!(nic.policy.allow.is_empty());
        nic.validate(0).expect("ok");
    }

    #[test]
    fn parses_flat_policy_fields() {
        let nic: Nic = toml::from_str(
            r#"
backend = "user"
allow_lan = false
anti_spoof = false
bandwidth_bps = 12500000
pps = 1000
allow = [{ cidr = "1.1.1.1/32", port = 443, proto = "tcp" }]
deny = [{ cidr = "8.8.8.8/32" }]
"#,
        )
        .expect("parse");
        nic.validate(0).expect("ok");
        assert!(!nic.policy.allow_lan);
        assert!(!nic.policy.anti_spoof);
        assert_eq!(nic.policy.bandwidth_bps, Some(12_500_000));
        assert_eq!(nic.policy.pps, Some(1000));
        assert_eq!(
            nic.policy.allow[0],
            CidrPort {
                cidr: "1.1.1.1/32".into(),
                port: Some(443),
                proto: Some(crate::ForwardProto::Tcp),
            }
        );
    }

    #[test]
    fn rejects_bad_cidr_and_zero_rates() {
        let nic: Nic = toml::from_str(
            r#"
backend = "user"
deny = [{ cidr = "10.0.0.0" }]
"#,
        )
        .expect("parse");
        assert!(nic.validate(0).is_err());
        let nic: Nic = toml::from_str("backend = \"user\"\npps = 0\n").expect("parse");
        assert!(nic.validate(0).is_err());
    }

    #[test]
    fn cidr_masks() {
        let (net, mask) = parse_cidr("10.1.2.3/8").expect("cidr");
        assert_eq!(mask, 0xff00_0000);
        assert_eq!(net, 0x0a00_0000);
        assert!(parse_cidr("1.2.3.4/33").is_err());
    }
}
