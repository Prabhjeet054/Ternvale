//! Ethernet frame summaries for TRACE logs, and the Internet checksum.

use super::mac_str;

/// EtherType for IPv4.
pub const ETHERTYPE_IPV4: u16 = 0x0800;
/// EtherType for ARP.
pub const ETHERTYPE_ARP: u16 = 0x0806;

/// Layer-3 details recognised in a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// ARP with opcode and sender/target protocol addresses.
    Arp {
        /// 1 = request, 2 = reply.
        op: u16,
        /// Sender IPv4 address.
        spa: [u8; 4],
        /// Target IPv4 address.
        tpa: [u8; 4],
    },
    /// IPv4 with protocol, addresses, and ICMP type/code when protocol is 1.
    Ipv4 {
        /// IP protocol number.
        proto: u8,
        /// Source address.
        src: [u8; 4],
        /// Destination address.
        dst: [u8; 4],
        /// IPv4 total length field.
        total_len: u16,
        /// ICMP `(type, code)` for protocol 1.
        icmp: Option<(u8, u8)>,
    },
    /// Anything else, including truncated ARP/IPv4.
    Other,
}

/// What a TRACE line says about one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// Destination MAC.
    pub dst: [u8; 6],
    /// Source MAC.
    pub src: [u8; 6],
    /// EtherType field.
    pub ethertype: u16,
    /// Frame length in bytes.
    pub len: usize,
    /// Layer-3 details.
    pub kind: Kind,
}

/// Parse the Ethernet header and, where present, ARP or IPv4. `None` for runts.
#[tracing::instrument(level = "trace", target = "ternvale::net", skip_all, fields(len = frame.len()))]
pub fn summarize(frame: &[u8]) -> Option<Summary> {
    if frame.len() < 14 {
        return None;
    }
    let mut dst = [0u8; 6];
    let mut src = [0u8; 6];
    dst.copy_from_slice(&frame[0..6]);
    src.copy_from_slice(&frame[6..12]);
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    let body = &frame[14..];
    let kind = match ethertype {
        ETHERTYPE_ARP if body.len() >= 28 => Kind::Arp {
            op: u16::from_be_bytes([body[6], body[7]]),
            spa: ip4(&body[14..18]),
            tpa: ip4(&body[24..28]),
        },
        ETHERTYPE_IPV4 if body.len() >= 20 && body[0] >> 4 == 4 => {
            let ihl = usize::from(body[0] & 0x0f) * 4;
            let proto = body[9];
            let icmp = (proto == 1)
                .then(|| body.get(ihl..ihl + 2))
                .flatten()
                .map(|t| (t[0], t[1]));
            Kind::Ipv4 {
                proto,
                src: ip4(&body[12..16]),
                dst: ip4(&body[16..20]),
                total_len: u16::from_be_bytes([body[2], body[3]]),
                icmp,
            }
        }
        _ => Kind::Other,
    };
    Some(Summary {
        dst,
        src,
        ethertype,
        len: frame.len(),
        kind,
    })
}

/// One TRACE line per frame on `ternvale::net`. Does nothing unless TRACE is on.
pub(super) fn trace(direction: &'static str, frame: &[u8]) {
    if !tracing::enabled!(target: "ternvale::net", tracing::Level::TRACE) {
        return;
    }
    let Some(s) = summarize(frame) else {
        tracing::trace!(target: "ternvale::net", direction, len = frame.len(), "net runt frame");
        return;
    };
    let ethertype = format!("{:#06x}", s.ethertype);
    let (src_mac, dst_mac) = (mac_str(&s.src), mac_str(&s.dst));
    match s.kind {
        Kind::Arp { op, spa, tpa } => tracing::trace!(
            target: "ternvale::net",
            direction, len = s.len, %ethertype, %src_mac, %dst_mac,
            arp_op = op, spa = %ip_str(spa), tpa = %ip_str(tpa),
            "net packet"
        ),
        Kind::Ipv4 {
            proto,
            src,
            dst,
            total_len,
            icmp: Some((icmp_type, icmp_code)),
        } => tracing::trace!(
            target: "ternvale::net",
            direction, len = s.len, %ethertype, %src_mac, %dst_mac,
            ip_proto = proto, src_ip = %ip_str(src), dst_ip = %ip_str(dst), ip_len = total_len,
            icmp_type, icmp_code,
            "net packet"
        ),
        Kind::Ipv4 {
            proto,
            src,
            dst,
            total_len,
            icmp: None,
        } => tracing::trace!(
            target: "ternvale::net",
            direction, len = s.len, %ethertype, %src_mac, %dst_mac,
            ip_proto = proto, src_ip = %ip_str(src), dst_ip = %ip_str(dst), ip_len = total_len,
            "net packet"
        ),
        Kind::Other => tracing::trace!(
            target: "ternvale::net",
            direction, len = s.len, %ethertype, %src_mac, %dst_mac,
            "net packet"
        ),
    }
}

/// RFC 1071 ones'-complement sum. A buffer with a valid checksum field sums to 0.
pub(super) fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    for pair in bytes.chunks(2) {
        let word = match pair {
            [hi, lo] => u16::from_be_bytes([*hi, *lo]),
            [hi] => u16::from_be_bytes([*hi, 0]),
            _ => 0,
        };
        sum += u32::from(word);
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn ip4(bytes: &[u8]) -> [u8; 4] {
    let mut out = [0u8; 4];
    out.copy_from_slice(bytes);
    out
}

fn ip_str(ip: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{arp_request, echo_request, GUEST_IP};
    use super::{checksum, summarize, Kind, ETHERTYPE_ARP, ETHERTYPE_IPV4};

    #[test]
    fn summarizes_arp_and_icmp() {
        let arp = summarize(&arp_request([10, 0, 2, 2])).expect("arp");
        assert_eq!(arp.ethertype, ETHERTYPE_ARP);
        assert_eq!(arp.len, 42);
        assert_eq!(
            arp.kind,
            Kind::Arp {
                op: 1,
                spa: GUEST_IP,
                tpa: [10, 0, 2, 2]
            }
        );
        let ping = summarize(&echo_request(1, b"abcd")).expect("icmp");
        assert_eq!(ping.ethertype, ETHERTYPE_IPV4);
        match ping.kind {
            Kind::Ipv4 {
                proto,
                icmp,
                total_len,
                ..
            } => {
                assert_eq!(proto, 1);
                assert_eq!(icmp, Some((8, 0)));
                assert_eq!(total_len, 32);
            }
            other => panic!("expected ipv4, got {other:?}"),
        }
    }

    #[test]
    fn runts_and_unknown_types() {
        assert!(summarize(&[0u8; 13]).is_none());
        let mut frame = vec![0u8; 20];
        frame[12..14].copy_from_slice(&0x86ddu16.to_be_bytes());
        assert_eq!(summarize(&frame).expect("frame").kind, Kind::Other);
    }

    #[test]
    fn trace_logs_ethertype_size_and_arp_fields() {
        let dir = std::env::temp_dir().join(format!("ternvale-net-trace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("log dir");
        let mut config = ternvale_log::LogConfig::new("net-trace", dir.clone());
        config.level = "ternvale::net=trace".to_string();
        let guard = ternvale_log::init(config).expect("log");
        super::trace("tx", &arp_request([10, 0, 2, 2]));
        super::trace("rx", &echo_request(9, b"x"));
        let path = guard.log_path().to_path_buf();
        drop(guard);
        let text = std::fs::read_to_string(&path).expect("log");
        std::fs::remove_dir_all(&dir).expect("remove log dir");
        assert!(text.contains("net packet"), "{text}");
        assert!(
            text.contains("ethertype=0x0806") && text.contains("arp_op=1"),
            "{text}"
        );
        assert!(
            text.contains("len=42") && text.contains("tpa=10.0.2.2"),
            "{text}"
        );
        assert!(
            text.contains("ethertype=0x0800") && text.contains("icmp_type=8"),
            "{text}"
        );
        assert!(text.contains("dst_ip=10.0.2.2"), "{text}");
    }

    #[test]
    fn checksum_matches_rfc1071_example() {
        let data = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
        assert_eq!(checksum(&data), !0xddf2);
        assert_eq!(checksum(&[0xab]), !0xab00);
    }
}
