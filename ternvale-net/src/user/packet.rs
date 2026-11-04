//! Ethernet / IPv4 / UDP / TCP / ICMP headers for the user-mode NAT.

pub const ETH_HLEN: usize = 14;
pub const IPV4_HLEN: usize = 20;
pub const UDP_HLEN: usize = 8;
pub const TCP_HLEN: usize = 20;
pub const ICMP_HLEN: usize = 8;
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const IP_PROTO_ICMP: u8 = 1;
pub const IP_PROTO_TCP: u8 = 6;
pub const IP_PROTO_UDP: u8 = 17;
pub const TCP_FIN: u8 = 0x01;
pub const TCP_SYN: u8 = 0x02;
pub const TCP_RST: u8 = 0x04;
pub const TCP_PSH: u8 = 0x08;
pub const TCP_ACK: u8 = 0x10;
pub const REPLY_TTL: u8 = 64;

/// Internet checksum (RFC 1071).
#[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all)]
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut chunks = data.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u32::from(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    if let Some(odd) = chunks.remainder().first() {
        sum += u32::from(*odd) << 8;
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !sum as u16
}

/// Parsed IPv4 datagram on Ethernet (no options, unfragmented).
#[derive(Debug, Clone, Copy)]
pub struct Ipv4<'a> {
    pub src_mac: [u8; 6],
    pub src: [u8; 4],
    pub dst: [u8; 4],
    pub proto: u8,
    pub payload: &'a [u8],
}

/// Split an Ethernet frame into IPv4 if it is a clean unfragmented packet.
#[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all, fields(len = frame.len()))]
pub fn parse_ipv4(frame: &[u8]) -> Option<Ipv4<'_>> {
    if frame.len() < ETH_HLEN + IPV4_HLEN {
        return None;
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    if ethertype != ETHERTYPE_IPV4 {
        return None;
    }
    let ip = &frame[ETH_HLEN..];
    if ip[0] >> 4 != 4 {
        return None;
    }
    let ihl = usize::from(ip[0] & 0x0f) * 4;
    if ihl != IPV4_HLEN {
        return None;
    }
    let total = usize::from(u16::from_be_bytes([ip[2], ip[3]]));
    if total < ihl || ETH_HLEN + total > frame.len() {
        return None;
    }
    let frag = u16::from_be_bytes([ip[6], ip[7]]);
    if frag & 0x3fff != 0 {
        return None;
    }
    if checksum(&ip[..ihl]) != 0 {
        tracing::debug!(target: "ternvale::net::user", "dropped ipv4 with bad checksum");
        return None;
    }
    let mut src_mac = [0u8; 6];
    src_mac.copy_from_slice(&frame[6..12]);
    Some(Ipv4 {
        src_mac,
        src: [ip[12], ip[13], ip[14], ip[15]],
        dst: [ip[16], ip[17], ip[18], ip[19]],
        proto: ip[9],
        payload: &ip[ihl..total],
    })
}

/// Ethernet + IPv4 header in front of `payload` (`proto` is the IP protocol).
#[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all)]
pub fn ipv4_frame(
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    src: [u8; 4],
    dst: [u8; 4],
    proto: u8,
    ip_id: u16,
    payload: &[u8],
) -> Vec<u8> {
    let total = IPV4_HLEN + payload.len();
    let mut out = Vec::with_capacity(ETH_HLEN + total);
    out.extend_from_slice(&dst_mac);
    out.extend_from_slice(&src_mac);
    out.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    let mut header = [0u8; IPV4_HLEN];
    header[0] = 0x45;
    header[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    header[4..6].copy_from_slice(&ip_id.to_be_bytes());
    header[8] = REPLY_TTL;
    header[9] = proto;
    header[12..16].copy_from_slice(&src);
    header[16..20].copy_from_slice(&dst);
    let sum = checksum(&header);
    header[10..12].copy_from_slice(&sum.to_be_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(payload);
    out
}

/// UDP header + payload with a correct UDP checksum (IPv4 pseudo-header).
#[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all)]
pub fn udp_payload(
    src: [u8; 4],
    dst: [u8; 4],
    src_port: u16,
    dst_port: u16,
    data: &[u8],
) -> Vec<u8> {
    let len = UDP_HLEN + data.len();
    let mut udp = vec![0u8; len];
    udp[0..2].copy_from_slice(&src_port.to_be_bytes());
    udp[2..4].copy_from_slice(&dst_port.to_be_bytes());
    udp[4..6].copy_from_slice(&(len as u16).to_be_bytes());
    udp[8..].copy_from_slice(data);
    let mut pseudo = Vec::with_capacity(12 + len);
    pseudo.extend_from_slice(&src);
    pseudo.extend_from_slice(&dst);
    pseudo.extend_from_slice(&[0, IP_PROTO_UDP]);
    pseudo.extend_from_slice(&(len as u16).to_be_bytes());
    pseudo.extend_from_slice(&udp);
    let sum = checksum(&pseudo);
    let sum = if sum == 0 { 0xffff } else { sum };
    udp[6..8].copy_from_slice(&sum.to_be_bytes());
    udp
}

/// TCP header with options-free 20-byte header.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all)]
pub fn tcp_payload(
    src: [u8; 4],
    dst: [u8; 4],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    data: &[u8],
) -> Vec<u8> {
    let len = TCP_HLEN + data.len();
    let mut tcp = vec![0u8; len];
    tcp[0..2].copy_from_slice(&src_port.to_be_bytes());
    tcp[2..4].copy_from_slice(&dst_port.to_be_bytes());
    tcp[4..8].copy_from_slice(&seq.to_be_bytes());
    tcp[8..12].copy_from_slice(&ack.to_be_bytes());
    tcp[12] = 0x50;
    tcp[13] = flags;
    tcp[14..16].copy_from_slice(&window.to_be_bytes());
    tcp[TCP_HLEN..].copy_from_slice(data);
    let mut pseudo = Vec::with_capacity(12 + len);
    pseudo.extend_from_slice(&src);
    pseudo.extend_from_slice(&dst);
    pseudo.extend_from_slice(&[0, IP_PROTO_TCP]);
    pseudo.extend_from_slice(&(len as u16).to_be_bytes());
    pseudo.extend_from_slice(&tcp);
    let sum = checksum(&pseudo);
    tcp[16..18].copy_from_slice(&sum.to_be_bytes());
    tcp
}

/// Parsed UDP header.
#[derive(Debug, Clone, Copy)]
pub struct UdpHdr<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub data: &'a [u8],
}

#[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all)]
pub fn parse_udp(payload: &[u8]) -> Option<UdpHdr<'_>> {
    if payload.len() < UDP_HLEN {
        return None;
    }
    let len = usize::from(u16::from_be_bytes([payload[4], payload[5]]));
    if len < UDP_HLEN || len > payload.len() {
        return None;
    }
    Some(UdpHdr {
        src_port: u16::from_be_bytes([payload[0], payload[1]]),
        dst_port: u16::from_be_bytes([payload[2], payload[3]]),
        data: &payload[UDP_HLEN..len],
    })
}

/// Parsed TCP header (data offset honored; options skipped).
#[derive(Debug, Clone, Copy)]
pub struct TcpHdr<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub flags: u8,
    pub data: &'a [u8],
}

#[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all)]
pub fn parse_tcp(payload: &[u8]) -> Option<TcpHdr<'_>> {
    if payload.len() < TCP_HLEN {
        return None;
    }
    let doff = usize::from(payload[12] >> 4) * 4;
    if doff < TCP_HLEN || doff > payload.len() {
        return None;
    }
    Some(TcpHdr {
        src_port: u16::from_be_bytes([payload[0], payload[1]]),
        dst_port: u16::from_be_bytes([payload[2], payload[3]]),
        seq: u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]),
        flags: payload[13],
        data: &payload[doff..],
    })
}

#[cfg(test)]
mod tests {
    use super::{checksum, parse_ipv4, udp_payload, IPV4_HLEN};

    #[test]
    fn checksum_zero_on_valid_header() {
        let mut h = [0u8; IPV4_HLEN];
        h[0] = 0x45;
        h[2] = 0;
        h[3] = 20;
        h[8] = 64;
        h[9] = 17;
        let sum = checksum(&h);
        h[10..12].copy_from_slice(&sum.to_be_bytes());
        assert_eq!(checksum(&h), 0);
    }

    #[test]
    fn round_trip_udp_frame() {
        let udp = udp_payload([10, 0, 2, 15], [10, 0, 2, 2], 1234, 67, b"hi");
        assert_eq!(udp.len(), 8 + 2);
        let frame = super::ipv4_frame([1; 6], [2; 6], [10, 0, 2, 15], [10, 0, 2, 2], 17, 1, &udp);
        let ip = parse_ipv4(&frame).expect("ip");
        assert_eq!(ip.proto, 17);
        assert_eq!(ip.src, [10, 0, 2, 15]);
    }

    #[test]
    fn parser_rejects_malformed_without_panic() {
        let mut seed = 1u64;
        for _ in 0..256 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let n = (seed % 64) as usize;
            let mut buf = vec![0u8; n];
            for (i, b) in buf.iter_mut().enumerate() {
                *b = seed.to_le_bytes()[i % 8];
            }
            let _ = parse_ipv4(&buf);
            let _ = super::parse_udp(&buf);
            let _ = super::parse_tcp(&buf);
            let _ = checksum(&buf);
        }
    }
}
