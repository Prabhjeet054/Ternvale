//! Frame builders shared by virtio-net unit tests.

use super::loopback::{GATEWAY_IP, GATEWAY_MAC};
use super::packet::checksum;

pub const GUEST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
pub const GUEST_IP: [u8; 4] = [10, 0, 2, 15];

/// Broadcast ARP request from the guest asking for `target`.
pub fn arp_request(target: [u8; 4]) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(&[0xff; 6]);
    f.extend_from_slice(&GUEST_MAC);
    f.extend_from_slice(&[0x08, 0x06]);
    f.extend_from_slice(&[0, 1, 0x08, 0x00, 6, 4, 0, 1]);
    f.extend_from_slice(&GUEST_MAC);
    f.extend_from_slice(&GUEST_IP);
    f.extend_from_slice(&[0; 6]);
    f.extend_from_slice(&target);
    f
}

/// ICMP echo request from the guest to the gateway.
pub fn echo_request(seq: u16, payload: &[u8]) -> Vec<u8> {
    echo_request_to(GATEWAY_IP, seq, payload)
}

/// ICMP echo request from the guest to `dst`, with valid checksums.
pub fn echo_request_to(dst: [u8; 4], seq: u16, payload: &[u8]) -> Vec<u8> {
    let mut icmp = vec![8, 0, 0, 0, 0x12, 0x34];
    icmp.extend_from_slice(&seq.to_be_bytes());
    icmp.extend_from_slice(payload);
    let sum = checksum(&icmp);
    icmp[2..4].copy_from_slice(&sum.to_be_bytes());
    let total = (20 + icmp.len()) as u16;
    let mut ip = vec![0x45, 0];
    ip.extend_from_slice(&total.to_be_bytes());
    ip.extend_from_slice(&[0, 1, 0x40, 0, 64, 1, 0, 0]);
    ip.extend_from_slice(&GUEST_IP);
    ip.extend_from_slice(&dst);
    let sum = checksum(&ip);
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    let mut f = Vec::new();
    f.extend_from_slice(&GATEWAY_MAC);
    f.extend_from_slice(&GUEST_MAC);
    f.extend_from_slice(&[0x08, 0x00]);
    f.extend_from_slice(&ip);
    f.extend_from_slice(&icmp);
    f
}
