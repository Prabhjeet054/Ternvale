//! User-mode NAT: DHCP, DNS, TCP/UDP/ICMP without root or entitlements.

mod dhcp;
mod dns;
mod flow;
mod forward;
mod icmp;
mod packet;
mod tcp;
mod tcp_in;
mod udp;

pub use icmp::icmp_available;

use std::collections::{HashMap, VecDeque};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::user::flow::{Flow, FlowKey, Listener};
use crate::user::packet::{parse_ipv4, ETHERTYPE_ARP, IP_PROTO_ICMP, IP_PROTO_TCP, IP_PROTO_UDP};

/// Virtual gateway (QEMU user-mode compatible).
pub const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
/// Built-in DNS address.
pub const DNS_IP: [u8; 4] = [10, 0, 2, 3];
/// Address the DHCP server always leases.
pub const GUEST_IP: [u8; 4] = [10, 0, 2, 15];
/// `/24` on `10.0.2.0`.
pub const NETMASK: [u8; 4] = [255, 255, 255, 0];
/// Gateway MAC (locally administered).
pub const GATEWAY_MAC: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
/// Default guest MAC until DHCP/ARP names one.
pub const DEFAULT_GUEST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

pub(crate) const DHCP_SERVER_PORT: u16 = 67;
pub(crate) const DHCP_CLIENT_PORT: u16 = 68;
pub(crate) const DNS_PORT: u16 = 53;
pub(crate) const MAX_FLOWS: usize = 512;
pub(crate) const MAX_QUEUED: usize = 128;
pub(crate) const ORDER: Ordering = Ordering::Relaxed;

/// TCP or UDP port forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Forward {
    /// `tcp` or `udp`.
    pub proto: Proto,
    /// Host bind address (usually `127.0.0.1`).
    pub host: Ipv4Addr,
    /// Host port.
    pub host_port: u16,
    /// Guest port on [`GUEST_IP`].
    pub guest_port: u16,
}

/// Forward protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

/// Counters for flows, bytes, and drops.
#[derive(Debug, Default)]
pub struct UserStats {
    /// Flows created.
    pub flows: AtomicU64,
    /// Payload bytes host→guest.
    pub bytes_in: AtomicU64,
    /// Payload bytes guest→host.
    pub bytes_out: AtomicU64,
    /// Dropped packets / failed sockets.
    pub drops: AtomicU64,
}

/// In-process slirp-style NAT.
pub struct UserNet {
    pub(crate) rx: VecDeque<Vec<u8>>,
    pub(crate) flows: HashMap<FlowKey, Flow>,
    pub(crate) listeners: Vec<Listener>,
    pub(crate) guest_mac: [u8; 6],
    pub(crate) gateway_mac: [u8; 6],
    pub(crate) stats: UserStats,
    pub(crate) icmp_ok: bool,
    ip_id: u16,
    isn: u32,
    host_port: u16,
    last_live: usize,
}

impl UserNet {
    /// Bind forwards and start with an empty flow table.
    #[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all, fields(forwards = forwards.len()))]
    pub fn new(forwards: Vec<Forward>) -> Result<Self, UserError> {
        let listeners = forward::bind_all(&forwards)?;
        let icmp_ok = icmp::icmp_available();
        tracing::info!(
            target: "ternvale::net::user",
            gateway = %ip_str(GATEWAY_IP),
            guest = %ip_str(GUEST_IP),
            dns = %ip_str(DNS_IP),
            forwards = listeners.len(),
            icmp = icmp_ok,
            "user-mode NAT ready"
        );
        Ok(Self {
            rx: VecDeque::new(),
            flows: HashMap::new(),
            listeners,
            guest_mac: DEFAULT_GUEST_MAC,
            gateway_mac: GATEWAY_MAC,
            stats: UserStats::default(),
            icmp_ok,
            ip_id: 1,
            isn: 1,
            host_port: 50_000,
            last_live: 0,
        })
    }

    /// Guest TX: ARP, DHCP, DNS, then NAT.
    #[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all, fields(len = frame.len()))]
    pub fn send(&mut self, frame: &[u8]) -> Result<(), UserError> {
        if frame.len() < 14 {
            return Ok(());
        }
        let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
        if ethertype == ETHERTYPE_ARP {
            arp_reply(self, frame);
            return Ok(());
        }
        if ethertype != packet::ETHERTYPE_IPV4 {
            return Ok(());
        }
        let Some(ip) = parse_ipv4(frame) else {
            self.drop_reason("bad ipv4");
            return Ok(());
        };
        match ip.proto {
            IP_PROTO_UDP => {
                if let Some(udp) = packet::parse_udp(ip.payload) {
                    if udp.dst_port == DHCP_SERVER_PORT {
                        dhcp::handle(self, &ip);
                        return Ok(());
                    }
                    if udp.dst_port == DNS_PORT && (ip.dst == DNS_IP || ip.dst == GATEWAY_IP) {
                        dns::handle(self, &ip);
                        return Ok(());
                    }
                }
                udp::handle(self, &ip);
            }
            IP_PROTO_TCP => tcp::handle(self, &ip),
            IP_PROTO_ICMP => icmp::handle(self, &ip),
            other => {
                tracing::debug!(target: "ternvale::net::user", proto = other, "ignored ip proto");
            }
        }
        self.log_conntrack();
        Ok(())
    }

    /// Host sockets → guest frames. Also expires idle flows.
    #[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all)]
    pub fn poll(&mut self) -> Option<Vec<u8>> {
        if self.rx.is_empty() {
            udp::poll(self);
            tcp::poll(self);
            icmp::poll(self);
            forward::poll(self);
            self.log_conntrack();
        }
        self.rx.pop_front()
    }

    /// Snapshot of counters.
    #[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
    pub fn stats(&self) -> (u64, u64, u64, u64) {
        (
            self.stats.flows.load(ORDER),
            self.stats.bytes_in.load(ORDER),
            self.stats.bytes_out.load(ORDER),
            self.stats.drops.load(ORDER),
        )
    }

    /// Number of live tracked flows (tests).
    #[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
    pub fn flow_count(&self) -> usize {
        self.flows.len()
    }

    pub(crate) fn queue(&mut self, frame: Vec<u8>) {
        if self.rx.len() >= MAX_QUEUED {
            self.drop_reason("rx queue full");
            return;
        }
        self.rx.push_back(frame);
    }

    pub(crate) fn drop_reason(&mut self, reason: &'static str) {
        tracing::warn!(target: "ternvale::net::user", reason, "packet drop");
        self.stats.drops.fetch_add(1, ORDER);
    }

    pub(crate) fn next_ip_id(&mut self) -> u16 {
        let id = self.ip_id;
        self.ip_id = self.ip_id.wrapping_add(1);
        id
    }

    pub(crate) fn next_isn(&mut self) -> u32 {
        let n = self.isn;
        self.isn = self.isn.wrapping_add(64000);
        n
    }

    pub(crate) fn next_host_port(&mut self) -> u16 {
        let p = self.host_port;
        self.host_port = if self.host_port == 60_000 {
            50_000
        } else {
            self.host_port + 1
        };
        p
    }

    pub(crate) fn log_conntrack(&mut self) {
        let live = self.flows.len();
        if live == self.last_live {
            return;
        }
        let created = self.stats.flows.load(ORDER);
        let drops = self.stats.drops.load(ORDER);
        if live == 0 {
            tracing::debug!(
                target: "ternvale::net::user",
                created,
                drops,
                live = 0u64,
                "conntrack empty"
            );
        } else {
            tracing::debug!(
                target: "ternvale::net::user",
                live,
                created,
                drops,
                "conntrack"
            );
        }
        self.last_live = live;
    }

    /// Guest `10.0.2.2` is the host loopback (QEMU slirp convention).
    #[inline]
    pub(crate) fn nat_v4(ip: [u8; 4]) -> Ipv4Addr {
        if ip == GATEWAY_IP {
            Ipv4Addr::LOCALHOST
        } else {
            Ipv4Addr::from(ip)
        }
    }

    pub(crate) fn learn_mac(&mut self, mac: [u8; 6]) {
        if self.guest_mac != mac {
            tracing::debug!(
                target: "ternvale::net::user",
                mac = %mac_str(&mac),
                "learned guest mac"
            );
            self.guest_mac = mac;
        }
    }
}

impl Drop for UserNet {
    fn drop(&mut self) {
        let (flows, bytes_in, bytes_out, drops) = self.stats();
        tracing::info!(
            target: "ternvale::net::user",
            flows,
            bytes_in,
            bytes_out,
            drops,
            "user-mode NAT stopped"
        );
    }
}

/// User NAT failures (bind, and nothing else at runtime).
#[derive(Debug, thiserror::Error)]
pub enum UserError {
    /// A port-forward bind failed.
    #[error("bind {addr}: {source}")]
    Bind {
        /// `host:port`.
        addr: String,
        /// OS error.
        source: std::io::Error,
    },
}

fn arp_reply(net: &mut UserNet, frame: &[u8]) {
    if frame.len() < 42 {
        return;
    }
    let arp = &frame[14..42];
    let op = u16::from_be_bytes([arp[6], arp[7]]);
    if op != 1 {
        return;
    }
    let tpa = [arp[24], arp[25], arp[26], arp[27]];
    if tpa != GATEWAY_IP && tpa != DNS_IP {
        return;
    }
    let sha = &arp[8..14];
    net.learn_mac(sha.try_into().unwrap_or(DEFAULT_GUEST_MAC));
    let mut out = Vec::with_capacity(42);
    out.extend_from_slice(sha);
    out.extend_from_slice(&GATEWAY_MAC);
    out.extend_from_slice(&ETHERTYPE_ARP.to_be_bytes());
    out.extend_from_slice(&[0, 1, 0x08, 0x00, 6, 4, 0, 2]);
    out.extend_from_slice(&GATEWAY_MAC);
    out.extend_from_slice(&tpa);
    out.extend_from_slice(sha);
    out.extend_from_slice(&arp[14..18]);
    net.queue(out);
}

pub(crate) fn ip_str(ip: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
}

fn mac_str(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

#[cfg(test)]
mod tests {
    use super::{packet, UserNet, GATEWAY_IP, GATEWAY_MAC};

    fn arp_for(tpa: [u8; 4]) -> Vec<u8> {
        let mut f = vec![0xffu8; 14];
        f[6..12].copy_from_slice(&[0x52, 0x54, 0, 0x12, 0x34, 0x56]);
        f[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
        let mut arp = vec![0u8; 28];
        arp[0..2].copy_from_slice(&1u16.to_be_bytes());
        arp[2..4].copy_from_slice(&0x0800u16.to_be_bytes());
        arp[4] = 6;
        arp[5] = 4;
        arp[7] = 1;
        arp[8..14].copy_from_slice(&[0x52, 0x54, 0, 0x12, 0x34, 0x56]);
        arp[14..18].copy_from_slice(&[10, 0, 2, 15]);
        arp[24..28].copy_from_slice(&tpa);
        f.extend_from_slice(&arp);
        f
    }

    #[test]
    fn arp_for_gateway() {
        let mut net = UserNet::new(Vec::new()).expect("net");
        net.send(&arp_for(GATEWAY_IP)).expect("send");
        let reply = net.poll().expect("arp");
        assert_eq!(&reply[6..12], &GATEWAY_MAC);
        assert_eq!(&reply[28..32], &GATEWAY_IP);
        assert!(packet::parse_ipv4(&reply).is_none());
    }

    #[test]
    fn malformed_frames_do_not_panic() {
        let mut net = UserNet::new(Vec::new()).expect("net");
        let mut seed = 0x9e37_79b9_u64;
        for _ in 0..512 {
            seed = seed
                .wrapping_mul(0x5851_f42d_4c95_7f2d)
                .wrapping_add(0x1405_7b7e_f767_814f);
            let len = (seed % 80) as usize;
            let mut frame = vec![0u8; len];
            for (i, b) in frame.iter_mut().enumerate() {
                *b = ((seed >> ((i % 8) * 8)) & 0xff) as u8;
            }
            net.send(&frame).expect("send");
            let _ = net.poll();
        }
        assert!(net.flow_count() <= super::MAX_FLOWS);
    }
}
