//! Guest-TX network policy: anti-spoof, LAN isolation, CIDR lists, token buckets.

use std::time::Instant;

use ternvale_config::{parse_cidr, CidrPort, ForwardProto, Nic, NicPolicy};

use crate::user::packet::{
    parse_ipv4, parse_udp, ETHERTYPE_ARP, ETHERTYPE_IPV4, ETH_HLEN, IP_PROTO_TCP, IP_PROTO_UDP,
};
use crate::user::{DEFAULT_GUEST_MAC, DNS_IP, GATEWAY_IP, GUEST_IP};

/// Ethernet type for IPv6 (IEEE 802).
const ETHERTYPE_IPV6: u16 = 0x86dd;
const DHCP_SERVER: u16 = 67;
const DHCP_CLIENT: u16 = 68;
const BOOTREPLY: u8 = 2;
const DHCP_YIADDR: usize = 16;
const DHCP_OPTIONS: usize = 240;
const OPT_ROUTER: u8 = 3;

/// Drop-counter names exported into metrics (`query-stats` `net_policy`).
pub const RULE_SPOOF_MAC: &str = "spoof_mac";
pub const RULE_SPOOF_IP: &str = "spoof_ip";
pub const RULE_ALLOW_LAN: &str = "allow_lan";
pub const RULE_DENY: &str = "deny";
pub const RULE_ALLOW: &str = "allow";
pub const RULE_PPS: &str = "pps";
pub const RULE_BANDWIDTH: &str = "bandwidth";

/// Compiled outbound policy plus token buckets.
pub struct PolicyEngine {
    allow_lan: bool,
    anti_spoof: bool,
    expected_mac: [u8; 6],
    expected_ip: Option<[u8; 4]>,
    gateways: Vec<[u8; 4]>,
    allow: Vec<Rule>,
    deny: Vec<Rule>,
    pps: Option<TokenBucket>,
    bandwidth: Option<TokenBucket>,
}

struct Rule {
    net: u32,
    mask: u32,
    port: Option<u16>,
    proto: Option<u8>,
}

struct TokenBucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Instant,
}

impl PolicyEngine {
    /// Compile `nic.policy`. User NAT pins the lease to `10.0.2.15`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::net::user",
        skip_all,
        fields(backend = %nic.backend, allow_lan = nic.policy.allow_lan)
    )]
    pub fn from_nic(nic: &Nic) -> Self {
        let expected_ip = if nic.backend == "user" {
            Some(GUEST_IP)
        } else {
            None
        };
        let mut gateways = vec![GATEWAY_IP, DNS_IP];
        if nic.backend != "user" {
            gateways.clear();
        }
        tracing::info!(
            target: "ternvale::net::user",
            backend = %nic.backend,
            allow_lan = nic.policy.allow_lan,
            anti_spoof = nic.policy.anti_spoof,
            allow = nic.policy.allow.len(),
            deny = nic.policy.deny.len(),
            pps = nic.policy.pps.unwrap_or(0),
            bandwidth_bps = nic.policy.bandwidth_bps.unwrap_or(0),
            "net policy ready"
        );
        Self::from_parts(&nic.policy, DEFAULT_GUEST_MAC, expected_ip, gateways)
    }

    fn from_parts(
        policy: &NicPolicy,
        mac: [u8; 6],
        expected_ip: Option<[u8; 4]>,
        gateways: Vec<[u8; 4]>,
    ) -> Self {
        Self {
            allow_lan: policy.allow_lan,
            anti_spoof: policy.anti_spoof,
            expected_mac: mac,
            expected_ip,
            gateways,
            allow: compile_rules(&policy.allow),
            deny: compile_rules(&policy.deny),
            pps: policy.pps.map(|pps| TokenBucket::new(f64::from(pps), 1.0)),
            bandwidth: policy
                .bandwidth_bps
                .map(|bps| TokenBucket::new((bps as f64) / 8.0, 1.0)),
        }
    }

    /// Virtio config MAC used for anti-spoof (may differ per NIC index).
    #[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
    pub fn set_mac(&mut self, mac: [u8; 6]) {
        tracing::debug!(
            target: "ternvale::net::user",
            mac = %mac_str(&mac),
            "policy expected MAC"
        );
        self.expected_mac = mac;
    }

    /// Rule name when the guest TX frame is dropped.
    #[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all, fields(len = frame.len()))]
    pub fn decide_tx(&mut self, frame: &[u8]) -> Option<&'static str> {
        let rule = self.classify_tx(frame)?;
        tracing::debug!(
            target: "ternvale::net::user",
            rule,
            len = frame.len(),
            "policy drop"
        );
        Some(rule)
    }

    fn classify_tx(&mut self, frame: &[u8]) -> Option<&'static str> {
        if frame.len() < ETH_HLEN {
            return None;
        }
        if let Some(pps) = self.pps.as_mut() {
            if !pps.take(1.0) {
                return Some(RULE_PPS);
            }
        }
        if let Some(bw) = self.bandwidth.as_mut() {
            if !bw.take(frame.len() as f64) {
                return Some(RULE_BANDWIDTH);
            }
        }
        let src_mac = [frame[6], frame[7], frame[8], frame[9], frame[10], frame[11]];
        if self.anti_spoof && src_mac != self.expected_mac {
            return Some(RULE_SPOOF_MAC);
        }
        let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
        if ethertype == ETHERTYPE_ARP {
            return None;
        }
        if ethertype == ETHERTYPE_IPV6 {
            if !self.allow_lan {
                return Some(RULE_ALLOW_LAN);
            }
            return None;
        }
        if ethertype != ETHERTYPE_IPV4 {
            return None;
        }
        let ip = parse_ipv4(frame)?;
        let (dst_port, proto) = l4(&ip);
        if is_dhcp(&ip, dst_port) {
            return None;
        }
        if self.anti_spoof {
            if let Some(lease) = self.expected_ip {
                if ip.src != lease && ip.src != [0, 0, 0, 0] {
                    return Some(RULE_SPOOF_IP);
                }
            }
        }
        if !self.allow_lan && is_blocked_lan(ip.dst, &self.gateways) {
            return Some(RULE_ALLOW_LAN);
        }
        if matches_list(&self.deny, ip.dst, dst_port, proto) {
            return Some(RULE_DENY);
        }
        if !self.allow.is_empty() && !matches_list(&self.allow, ip.dst, dst_port, proto) {
            return Some(RULE_ALLOW);
        }
        None
    }

    /// Learn the vmnet DHCP lease and router from a host→guest frame.
    #[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all, fields(len = frame.len()))]
    pub fn observe_rx(&mut self, frame: &[u8]) {
        let Some((yiaddr, routers)) = dhcp_ack(frame) else {
            return;
        };
        if yiaddr != [0, 0, 0, 0] {
            tracing::info!(
                target: "ternvale::net::user",
                ip = %ip_str(&yiaddr),
                "policy learned guest lease"
            );
            self.expected_ip = Some(yiaddr);
        }
        for router in routers {
            if !self.gateways.contains(&router) {
                tracing::debug!(
                    target: "ternvale::net::user",
                    ip = %ip_str(&router),
                    "policy learned gateway"
                );
                self.gateways.push(router);
            }
        }
    }
}

fn compile_rules(list: &[CidrPort]) -> Vec<Rule> {
    list.iter()
        .filter_map(|rule| {
            let (net, mask) = parse_cidr(&rule.cidr).ok()?;
            let proto = match rule.proto {
                Some(ForwardProto::Tcp) => Some(IP_PROTO_TCP),
                Some(ForwardProto::Udp) => Some(IP_PROTO_UDP),
                None => None,
            };
            Some(Rule {
                net,
                mask,
                port: rule.port,
                proto,
            })
        })
        .collect()
}

fn matches_list(rules: &[Rule], dst: [u8; 4], port: Option<u16>, proto: u8) -> bool {
    let ip = u32::from_be_bytes(dst);
    rules.iter().any(|rule| {
        if ip & rule.mask != rule.net {
            return false;
        }
        if let Some(want) = rule.proto {
            if want != proto {
                return false;
            }
        }
        match (rule.port, port) {
            (None, _) => true,
            (Some(want), Some(have)) => want == have,
            (Some(_), None) => false,
        }
    })
}

fn is_blocked_lan(dst: [u8; 4], gateways: &[[u8; 4]]) -> bool {
    if gateways.contains(&dst) {
        return false;
    }
    let ip = u32::from_be_bytes(dst);
    const NETS: [(u32, u32); 4] = [
        (0x0a00_0000, 0xff00_0000), // 10.0.0.0/8
        (0xac10_0000, 0xfff0_0000), // 172.16.0.0/12
        (0xc0a8_0000, 0xffff_0000), // 192.168.0.0/16
        (0xa9fe_0000, 0xffff_0000), // 169.254.0.0/16
    ];
    NETS.iter().any(|(net, mask)| ip & mask == *net)
}

fn l4(ip: &crate::user::packet::Ipv4<'_>) -> (Option<u16>, u8) {
    match ip.proto {
        IP_PROTO_TCP if ip.payload.len() >= 4 => {
            let port = u16::from_be_bytes([ip.payload[2], ip.payload[3]]);
            (Some(port), IP_PROTO_TCP)
        }
        IP_PROTO_UDP => {
            let port = parse_udp(ip.payload).map(|udp| udp.dst_port);
            (port, IP_PROTO_UDP)
        }
        other => (None, other),
    }
}

fn is_dhcp(ip: &crate::user::packet::Ipv4<'_>, dst_port: Option<u16>) -> bool {
    ip.proto == IP_PROTO_UDP && matches!(dst_port, Some(DHCP_SERVER) | Some(DHCP_CLIENT))
}

fn dhcp_ack(frame: &[u8]) -> Option<([u8; 4], Vec<[u8; 4]>)> {
    let ip = parse_ipv4(frame)?;
    if ip.proto != IP_PROTO_UDP {
        return None;
    }
    let udp = parse_udp(ip.payload)?;
    if udp.dst_port != DHCP_CLIENT && udp.src_port != DHCP_SERVER {
        return None;
    }
    let bootp = udp.data;
    if bootp.len() < DHCP_OPTIONS || bootp[0] != BOOTREPLY {
        return None;
    }
    let mut yiaddr = [0u8; 4];
    yiaddr.copy_from_slice(&bootp[DHCP_YIADDR..DHCP_YIADDR + 4]);
    let mut routers = Vec::new();
    let mut i = DHCP_OPTIONS;
    while i + 1 < bootp.len() {
        let tag = bootp[i];
        if tag == 255 {
            break;
        }
        if tag == 0 {
            i += 1;
            continue;
        }
        let len = usize::from(bootp[i + 1]);
        let start = i + 2;
        let end = start.saturating_add(len);
        if end > bootp.len() {
            break;
        }
        if tag == OPT_ROUTER {
            for chunk in bootp[start..end].chunks_exact(4) {
                routers.push([chunk[0], chunk[1], chunk[2], chunk[3]]);
            }
        }
        i = end;
    }
    Some((yiaddr, routers))
}

impl TokenBucket {
    fn new(rate: f64, burst_secs: f64) -> Self {
        let burst = (rate * burst_secs).max(1.0);
        Self {
            rate,
            burst,
            tokens: burst,
            last: Instant::now(),
        }
    }

    fn take(&mut self, amount: f64) -> bool {
        let now = Instant::now();
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.rate).min(self.burst);
        if self.tokens >= amount {
            self.tokens -= amount;
            true
        } else {
            false
        }
    }
}

fn mac_str(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn ip_str(ip: &[u8; 4]) -> String {
    format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
