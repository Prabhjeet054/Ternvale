//! Built-in DHCPv4 server: always leases [`crate::user::GUEST_IP`].

use super::packet::{self, parse_udp, Ipv4};
use super::{UserNet, DHCP_CLIENT_PORT, DHCP_SERVER_PORT, GATEWAY_IP, GUEST_IP, NETMASK};

const BOOTREQUEST: u8 = 1;
const BOOTREPLY: u8 = 2;
const DHCP_MAGIC: [u8; 4] = [99, 130, 83, 99];
const OPT_MSG: u8 = 53;
const OPT_SERVER: u8 = 54;
const OPT_LEASE: u8 = 51;
const OPT_MASK: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_END: u8 = 255;
const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;

/// Handle a BOOTP/DHCP packet to the gateway UDP 67.
#[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
pub fn handle(net: &mut UserNet, ip: &Ipv4<'_>) {
    let Some(udp) = parse_udp(ip.payload) else {
        net.drop_reason("dhcp runt");
        return;
    };
    if udp.dst_port != DHCP_SERVER_PORT {
        return;
    }
    if udp.data.len() < 240 || udp.data[0] != BOOTREQUEST {
        net.drop_reason("dhcp not bootrequest");
        return;
    }
    if udp.data[236..240] != DHCP_MAGIC {
        net.drop_reason("dhcp bad magic");
        return;
    }
    let xid = &udp.data[4..8];
    let chaddr = &udp.data[28..34];
    let msg = option(udp.data, OPT_MSG).and_then(|v| v.first().copied());
    let reply_type = match msg {
        Some(DISCOVER) | None => OFFER,
        Some(REQUEST) => ACK,
        other => {
            tracing::debug!(target: "ternvale::net::user", msg = ?other, "dhcp ignored");
            return;
        }
    };
    net.learn_mac(chaddr.try_into().unwrap_or(ip.src_mac));
    let mut bootp = vec![0u8; 240];
    bootp[0] = BOOTREPLY;
    bootp[1] = 1;
    bootp[2] = 6;
    bootp[4..8].copy_from_slice(xid);
    bootp[16..20].copy_from_slice(&GUEST_IP);
    bootp[20..24].copy_from_slice(&GATEWAY_IP);
    bootp[28..34].copy_from_slice(chaddr);
    bootp[236..240].copy_from_slice(&DHCP_MAGIC);
    let mut opts = Vec::new();
    opts.extend_from_slice(&[OPT_MSG, 1, reply_type]);
    opts.extend_from_slice(&[OPT_SERVER, 4]);
    opts.extend_from_slice(&GATEWAY_IP);
    opts.extend_from_slice(&[OPT_LEASE, 4, 0, 1, 0x51, 0x80]);
    opts.extend_from_slice(&[OPT_MASK, 4]);
    opts.extend_from_slice(&NETMASK);
    opts.extend_from_slice(&[OPT_ROUTER, 4]);
    opts.extend_from_slice(&GATEWAY_IP);
    opts.extend_from_slice(&[OPT_DNS, 4]);
    opts.extend_from_slice(&super::DNS_IP);
    opts.push(OPT_END);
    bootp.extend_from_slice(&opts);
    let udp = packet::udp_payload(
        GATEWAY_IP,
        [255, 255, 255, 255],
        DHCP_SERVER_PORT,
        DHCP_CLIENT_PORT,
        &bootp,
    );
    let frame = packet::ipv4_frame(
        chaddr.try_into().unwrap_or(ip.src_mac),
        net.gateway_mac,
        GATEWAY_IP,
        [255, 255, 255, 255],
        packet::IP_PROTO_UDP,
        net.next_ip_id(),
        &udp,
    );
    tracing::debug!(
        target: "ternvale::net::user",
        reply = reply_type,
        guest = %super::ip_str(GUEST_IP),
        "dhcp reply"
    );
    net.queue(frame);
}

fn option(bootp: &[u8], tag: u8) -> Option<&[u8]> {
    let mut i = 240;
    while i < bootp.len() {
        match bootp[i] {
            OPT_END => return None,
            0 => i += 1,
            t => {
                if i + 1 >= bootp.len() {
                    return None;
                }
                let len = usize::from(bootp[i + 1]);
                if i + 2 + len > bootp.len() {
                    return None;
                }
                if t == tag {
                    return Some(&bootp[i + 2..i + 2 + len]);
                }
                i += 2 + len;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{handle, DISCOVER, OPT_MSG};
    use crate::user::packet::{ipv4_frame, udp_payload, IP_PROTO_UDP};
    use crate::user::{UserNet, DHCP_CLIENT_PORT, DHCP_SERVER_PORT, GATEWAY_IP, GUEST_IP};

    fn discover() -> Vec<u8> {
        let mut bootp = vec![0u8; 240];
        bootp[0] = 1;
        bootp[1] = 1;
        bootp[2] = 6;
        bootp[28..34].copy_from_slice(&[0x52, 0x54, 0, 0x12, 0x34, 0x56]);
        bootp[236..240].copy_from_slice(&[99, 130, 83, 99]);
        bootp.extend_from_slice(&[OPT_MSG, 1, DISCOVER, 255]);
        let udp = udp_payload(
            [0, 0, 0, 0],
            [255, 255, 255, 255],
            DHCP_CLIENT_PORT,
            DHCP_SERVER_PORT,
            &bootp,
        );
        ipv4_frame(
            [0xff; 6],
            [0x52, 0x54, 0, 0x12, 0x34, 0x56],
            [0, 0, 0, 0],
            [255, 255, 255, 255],
            IP_PROTO_UDP,
            1,
            &udp,
        )
    }

    #[test]
    fn offers_fixed_guest_address() {
        let mut net = UserNet::new(Vec::new()).expect("net");
        let frame = discover();
        let ip = crate::user::packet::parse_ipv4(&frame).expect("ip");
        handle(&mut net, &ip);
        let reply = net.poll().expect("offer");
        let body = crate::user::packet::parse_ipv4(&reply).expect("ip");
        let udp = crate::user::packet::parse_udp(body.payload).expect("udp");
        assert_eq!(&udp.data[16..20], &GUEST_IP);
        assert_eq!(&udp.data[20..24], &GATEWAY_IP);
    }
}
