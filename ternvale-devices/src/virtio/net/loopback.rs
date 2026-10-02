//! In-process backend that plays a gateway at [`GATEWAY_IP`].
//!
//! It answers ARP requests for the gateway address and ICMP echo requests sent
//! to it. Every other frame is dropped. No host privileges or sockets are used.

use std::collections::VecDeque;

use super::backend::NetBackend;
use super::packet::{checksum, ETHERTYPE_ARP, ETHERTYPE_IPV4};
use super::{mac_str, NetError};

/// Fake gateway IPv4 address (QEMU user-mode networking uses the same one).
pub const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
/// Fake gateway MAC address.
pub const GATEWAY_MAC: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];

const MAX_QUEUED: usize = 64;
const ETH_HLEN: usize = 14;
const ARP_LEN: usize = 28;
const IP_PROTO_ICMP: u8 = 1;
const ICMP_ECHO_REPLY: u8 = 0;
const ICMP_ECHO_REQUEST: u8 = 8;
const REPLY_TTL: u8 = 64;

/// Answers ARP and ICMP echo for the fake gateway.
#[derive(Debug, Default)]
pub struct LoopbackBackend {
    replies: VecDeque<Vec<u8>>,
}

impl LoopbackBackend {
    /// Empty backend with no queued replies.
    #[tracing::instrument(level = "debug", target = "ternvale::net")]
    pub fn new() -> Self {
        tracing::debug!(
            target: "ternvale::net",
            gateway_ip = ?GATEWAY_IP,
            gateway_mac = %mac_str(&GATEWAY_MAC),
            "loopback backend created"
        );
        Self::default()
    }

    fn queue(&mut self, frame: Vec<u8>, what: &'static str) {
        if self.replies.len() >= MAX_QUEUED {
            tracing::warn!(
                target: "ternvale::net",
                queued = self.replies.len(),
                what,
                "loopback reply queue full; dropping reply"
            );
            return;
        }
        tracing::debug!(target: "ternvale::net", what, len = frame.len(), "loopback reply queued");
        self.replies.push_back(frame);
    }
}

impl NetBackend for LoopbackBackend {
    fn name(&self) -> &'static str {
        "loopback"
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), NetError> {
        if frame.len() < ETH_HLEN {
            tracing::trace!(target: "ternvale::net", len = frame.len(), "loopback ignored runt");
            return Ok(());
        }
        let reply = match u16::from_be_bytes([frame[12], frame[13]]) {
            ETHERTYPE_ARP => arp_reply(frame).map(|r| (r, "arp reply")),
            ETHERTYPE_IPV4 => echo_reply(frame).map(|r| (r, "icmp echo reply")),
            _ => None,
        };
        match reply {
            Some((frame, what)) => self.queue(frame, what),
            None => {
                tracing::trace!(target: "ternvale::net", len = frame.len(), "loopback ignored frame")
            }
        }
        Ok(())
    }

    fn recv(&mut self) -> Result<Option<Vec<u8>>, NetError> {
        Ok(self.replies.pop_front())
    }
}

/// Reply to an Ethernet/IPv4 ARP request whose target is the gateway.
pub(super) fn arp_reply(frame: &[u8]) -> Option<Vec<u8>> {
    let arp = frame.get(ETH_HLEN..ETH_HLEN + ARP_LEN)?;
    let htype = u16::from_be_bytes([arp[0], arp[1]]);
    let ptype = u16::from_be_bytes([arp[2], arp[3]]);
    let op = u16::from_be_bytes([arp[6], arp[7]]);
    if htype != 1 || ptype != ETHERTYPE_IPV4 || arp[4] != 6 || arp[5] != 4 || op != 1 {
        return None;
    }
    let (sha, spa, tpa) = (&arp[8..14], &arp[14..18], &arp[24..28]);
    if tpa != GATEWAY_IP {
        return None;
    }
    let mut out = Vec::with_capacity(ETH_HLEN + ARP_LEN);
    out.extend_from_slice(sha);
    out.extend_from_slice(&GATEWAY_MAC);
    out.extend_from_slice(&ETHERTYPE_ARP.to_be_bytes());
    out.extend_from_slice(&[0, 1, 0x08, 0x00, 6, 4, 0, 2]);
    out.extend_from_slice(&GATEWAY_MAC);
    out.extend_from_slice(&GATEWAY_IP);
    out.extend_from_slice(sha);
    out.extend_from_slice(spa);
    Some(out)
}

/// Reply to an unfragmented ICMP echo request addressed to the gateway.
pub(super) fn echo_reply(frame: &[u8]) -> Option<Vec<u8>> {
    let ip = frame.get(ETH_HLEN..)?;
    if ip.len() < 20 || ip[0] >> 4 != 4 {
        return None;
    }
    let ihl = usize::from(ip[0] & 0x0f) * 4;
    let total = usize::from(u16::from_be_bytes([ip[2], ip[3]]));
    if ihl < 20 || total < ihl + 8 || total > ip.len() {
        return None;
    }
    let frag = u16::from_be_bytes([ip[6], ip[7]]);
    if frag & 0x3fff != 0 || ip[9] != IP_PROTO_ICMP || ip[16..20] != GATEWAY_IP {
        return None;
    }
    if checksum(&ip[..ihl]) != 0 {
        tracing::debug!(target: "ternvale::net", "loopback dropped ipv4 header with bad checksum");
        return None;
    }
    let icmp = &ip[ihl..total];
    if icmp[0] != ICMP_ECHO_REQUEST || icmp[1] != 0 {
        return None;
    }
    if checksum(icmp) != 0 {
        tracing::debug!(target: "ternvale::net", "loopback dropped icmp echo with bad checksum");
        return None;
    }
    let mut out = Vec::with_capacity(ETH_HLEN + total);
    out.extend_from_slice(&frame[6..12]);
    out.extend_from_slice(&GATEWAY_MAC);
    out.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    let mut header = ip[..ihl].to_vec();
    header[8] = REPLY_TTL;
    header[10..12].fill(0);
    header[12..16].copy_from_slice(&GATEWAY_IP);
    header[16..20].copy_from_slice(&ip[12..16]);
    let sum = checksum(&header);
    header[10..12].copy_from_slice(&sum.to_be_bytes());
    out.extend_from_slice(&header);
    let mut body = icmp.to_vec();
    body[0] = ICMP_ECHO_REPLY;
    body[2..4].fill(0);
    let sum = checksum(&body);
    body[2..4].copy_from_slice(&sum.to_be_bytes());
    out.extend_from_slice(&body);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::super::backend::NetBackend;
    use super::super::packet::checksum;
    use super::super::testutil::{arp_request, echo_request, echo_request_to, GUEST_IP, GUEST_MAC};
    use super::{LoopbackBackend, GATEWAY_IP, GATEWAY_MAC};

    #[test]
    fn answers_arp_for_the_gateway_only() {
        let mut lo = LoopbackBackend::new();
        lo.send(&arp_request([10, 0, 2, 3])).expect("send");
        assert!(lo.recv().expect("recv").is_none(), "other address ignored");
        lo.send(&arp_request(GATEWAY_IP)).expect("send");
        let reply = lo.recv().expect("recv").expect("reply");
        assert_eq!(&reply[0..6], &GUEST_MAC);
        assert_eq!(&reply[6..12], &GATEWAY_MAC);
        assert_eq!(&reply[20..22], &[0, 2], "op is reply");
        assert_eq!(&reply[22..28], &GATEWAY_MAC);
        assert_eq!(&reply[28..32], &GATEWAY_IP);
        assert_eq!(&reply[38..42], &GUEST_IP);
    }

    #[test]
    fn echo_reply_swaps_addresses_and_has_valid_checksums() {
        let mut lo = LoopbackBackend::new();
        lo.send(&echo_request(7, b"ternvale-ping")).expect("send");
        let reply = lo.recv().expect("recv").expect("reply");
        let ip = &reply[14..];
        assert_eq!(&reply[0..6], &GUEST_MAC);
        assert_eq!(&ip[12..16], &GATEWAY_IP);
        assert_eq!(&ip[16..20], &GUEST_IP);
        assert_eq!(checksum(&ip[..20]), 0, "ipv4 header checksum");
        let icmp = &ip[20..];
        assert_eq!(icmp[0], 0, "echo reply");
        assert_eq!(u16::from_be_bytes([icmp[6], icmp[7]]), 7, "sequence kept");
        assert_eq!(&icmp[8..], b"ternvale-ping");
        assert_eq!(checksum(icmp), 0, "icmp checksum");
    }

    #[test]
    fn drops_echo_with_a_bad_checksum_or_wrong_target() {
        let mut lo = LoopbackBackend::new();
        let mut bad = echo_request(1, b"x");
        let last = bad.len() - 1;
        bad[last] ^= 0xff;
        lo.send(&bad).expect("send");
        lo.send(&echo_request_to([10, 0, 2, 9], 1, b"x"))
            .expect("send");
        lo.send(&[0u8; 10]).expect("runt");
        assert!(lo.recv().expect("recv").is_none());
    }
}
