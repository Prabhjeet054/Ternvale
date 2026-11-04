//! Table-driven guest-TX policy cases.

use super::{
    PolicyEngine, RULE_ALLOW_LAN, RULE_BANDWIDTH, RULE_DENY, RULE_PPS, RULE_SPOOF_IP,
    RULE_SPOOF_MAC,
};
use crate::user::packet::{
    checksum, ETHERTYPE_IPV4, ETH_HLEN, IPV4_HLEN, IP_PROTO_TCP, IP_PROTO_UDP,
};
use crate::user::{DEFAULT_GUEST_MAC, GATEWAY_IP, GUEST_IP};
use ternvale_config::{CidrPort, ForwardProto, Nic, NicPolicy};

struct Case {
    name: &'static str,
    policy: NicPolicy,
    src_mac: [u8; 6],
    src: [u8; 4],
    dst: [u8; 4],
    dport: u16,
    proto: u8,
    /// Send this many copies on one engine; assert the last verdict.
    sends: usize,
    want: Option<&'static str>,
}

fn tcp_frame(src_mac: [u8; 6], src: [u8; 4], dst: [u8; 4], dport: u16, proto: u8) -> Vec<u8> {
    let mut pkt = vec![0u8; ETH_HLEN + IPV4_HLEN + 20];
    pkt[6..12].copy_from_slice(&src_mac);
    pkt[12..14].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    pkt[ETH_HLEN] = 0x45;
    let total = (IPV4_HLEN + 20) as u16;
    pkt[ETH_HLEN + 2..ETH_HLEN + 4].copy_from_slice(&total.to_be_bytes());
    pkt[ETH_HLEN + 9] = proto;
    pkt[ETH_HLEN + 12..ETH_HLEN + 16].copy_from_slice(&src);
    pkt[ETH_HLEN + 16..ETH_HLEN + 20].copy_from_slice(&dst);
    let csum = checksum(&pkt[ETH_HLEN..ETH_HLEN + IPV4_HLEN]);
    pkt[ETH_HLEN + 10..ETH_HLEN + 12].copy_from_slice(&csum.to_be_bytes());
    pkt[ETH_HLEN + IPV4_HLEN + 2..ETH_HLEN + IPV4_HLEN + 4].copy_from_slice(&dport.to_be_bytes());
    pkt
}

fn user_engine(policy: NicPolicy) -> PolicyEngine {
    let nic = Nic {
        backend: "user".into(),
        policy,
        ..Nic::user()
    };
    PolicyEngine::from_nic(&nic)
}

fn cidr(cidr: &str, port: Option<u16>, proto: Option<ForwardProto>) -> CidrPort {
    CidrPort {
        cidr: cidr.into(),
        port,
        proto,
    }
}

fn typical() -> NicPolicy {
    NicPolicy {
        allow_lan: false,
        anti_spoof: true,
        deny: vec![
            cidr("224.0.0.0/4", None, None),
            cidr("0.0.0.0/0", Some(25), Some(ForwardProto::Tcp)),
        ],
        ..NicPolicy::default()
    }
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "allowed wan tcp",
            policy: typical(),
            src_mac: DEFAULT_GUEST_MAC,
            src: GUEST_IP,
            dst: [1, 1, 1, 1],
            dport: 443,
            proto: IP_PROTO_TCP,
            sends: 1,
            want: None,
        },
        Case {
            name: "allowed virtual gateway",
            policy: typical(),
            src_mac: DEFAULT_GUEST_MAC,
            src: GUEST_IP,
            dst: GATEWAY_IP,
            dport: 5201,
            proto: IP_PROTO_TCP,
            sends: 1,
            want: None,
        },
        Case {
            name: "denied by cidr",
            policy: NicPolicy {
                deny: vec![cidr("8.8.8.8/32", None, None)],
                ..NicPolicy::default()
            },
            src_mac: DEFAULT_GUEST_MAC,
            src: GUEST_IP,
            dst: [8, 8, 8, 8],
            dport: 53,
            proto: IP_PROTO_UDP,
            sends: 1,
            want: Some(RULE_DENY),
        },
        Case {
            name: "denied by port",
            policy: NicPolicy {
                deny: vec![cidr("1.1.1.1/32", Some(443), Some(ForwardProto::Tcp))],
                ..NicPolicy::default()
            },
            src_mac: DEFAULT_GUEST_MAC,
            src: GUEST_IP,
            dst: [1, 1, 1, 1],
            dport: 443,
            proto: IP_PROTO_TCP,
            sends: 1,
            want: Some(RULE_DENY),
        },
        Case {
            name: "same cidr other port allowed",
            policy: NicPolicy {
                deny: vec![cidr("1.1.1.1/32", Some(443), Some(ForwardProto::Tcp))],
                ..NicPolicy::default()
            },
            src_mac: DEFAULT_GUEST_MAC,
            src: GUEST_IP,
            dst: [1, 1, 1, 1],
            dport: 80,
            proto: IP_PROTO_TCP,
            sends: 1,
            want: None,
        },
        Case {
            name: "denied rfc1918 when allow_lan false",
            policy: typical(),
            src_mac: DEFAULT_GUEST_MAC,
            src: GUEST_IP,
            dst: [192, 168, 1, 1],
            dport: 80,
            proto: IP_PROTO_TCP,
            sends: 1,
            want: Some(RULE_ALLOW_LAN),
        },
        Case {
            name: "rate limited pps",
            policy: NicPolicy {
                pps: Some(1),
                anti_spoof: false,
                ..NicPolicy::default()
            },
            src_mac: DEFAULT_GUEST_MAC,
            src: GUEST_IP,
            dst: [1, 1, 1, 1],
            dport: 443,
            proto: IP_PROTO_TCP,
            sends: 2,
            want: Some(RULE_PPS),
        },
        Case {
            name: "rate limited bandwidth",
            policy: NicPolicy {
                bandwidth_bps: Some(8),
                anti_spoof: false,
                ..NicPolicy::default()
            },
            src_mac: DEFAULT_GUEST_MAC,
            src: GUEST_IP,
            dst: [1, 1, 1, 1],
            dport: 443,
            proto: IP_PROTO_TCP,
            sends: 1,
            want: Some(RULE_BANDWIDTH),
        },
        Case {
            name: "spoofed mac",
            policy: typical(),
            src_mac: [0x52, 0x54, 0x00, 0x00, 0x00, 0x99],
            src: GUEST_IP,
            dst: [1, 1, 1, 1],
            dport: 443,
            proto: IP_PROTO_TCP,
            sends: 1,
            want: Some(RULE_SPOOF_MAC),
        },
        Case {
            name: "spoofed ip",
            policy: typical(),
            src_mac: DEFAULT_GUEST_MAC,
            src: [10, 0, 2, 99],
            dst: [1, 1, 1, 1],
            dport: 443,
            proto: IP_PROTO_TCP,
            sends: 1,
            want: Some(RULE_SPOOF_IP),
        },
    ]
}

#[test]
fn table_driven_guest_tx() {
    for case in cases() {
        let mut eng = user_engine(case.policy);
        let pkt = tcp_frame(case.src_mac, case.src, case.dst, case.dport, case.proto);
        let mut got = None;
        for _ in 0..case.sends.max(1) {
            got = eng.decide_tx(&pkt);
        }
        assert_eq!(got, case.want, "{}", case.name);
    }
}
