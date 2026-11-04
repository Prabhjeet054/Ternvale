//! UDP NAT: one host datagram socket per guest 5-tuple.

use std::io::ErrorKind;
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::time::Instant;

use super::flow::{Flow, FlowKey, UdpFlow, UDP_IDLE};
use super::packet::{self, parse_udp, Ipv4, IP_PROTO_UDP};
use super::{ip_str, UserNet, ORDER};

/// Open (or reuse) a host UDP socket and send the guest datagram.
#[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
pub fn handle(net: &mut UserNet, ip: &Ipv4<'_>) {
    let Some(udp) = parse_udp(ip.payload) else {
        net.drop_reason("udp runt");
        return;
    };
    let key = FlowKey::udp(ip.src, udp.src_port, ip.dst, udp.dst_port);
    if !net.flows.contains_key(&key) {
        if net.flows.len() >= super::MAX_FLOWS {
            net.drop_reason("flow cap");
            return;
        }
        let sock = match UdpSocket::bind("0.0.0.0:0") {
            Ok(sock) => sock,
            Err(error) => {
                tracing::warn!(target: "ternvale::net::user", error = %error, reason = "udp bind", "packet drop");
                net.stats.drops.fetch_add(1, ORDER);
                return;
            }
        };
        if let Err(error) = sock.set_nonblocking(true) {
            tracing::warn!(target: "ternvale::net::user", error = %error, reason = "udp nonblocking", "packet drop");
            net.stats.drops.fetch_add(1, ORDER);
            return;
        }
        tracing::debug!(
            target: "ternvale::net::user",
            src = %ip_str(ip.src),
            sport = udp.src_port,
            dst = %ip_str(ip.dst),
            dport = udp.dst_port,
            "new udp flow"
        );
        net.stats.flows.fetch_add(1, ORDER);
        net.flows.insert(
            key,
            Flow::Udp(UdpFlow {
                sock,
                guest_mac: ip.src_mac,
                guest_ip: ip.src,
                guest_port: udp.src_port,
                remote_ip: ip.dst,
                remote_port: udp.dst_port,
                last: Instant::now(),
            }),
        );
    }
    let Some(Flow::Udp(flow)) = net.flows.get_mut(&key) else {
        return;
    };
    flow.last = Instant::now();
    let dest = SocketAddr::V4(SocketAddrV4::new(UserNet::nat_v4(ip.dst), udp.dst_port));
    match flow.sock.send_to(udp.data, dest) {
        Ok(n) => {
            net.stats.bytes_out.fetch_add(n as u64, ORDER);
        }
        Err(error) => {
            tracing::warn!(target: "ternvale::net::user", error = %error, reason = "udp send", "packet drop");
            net.stats.drops.fetch_add(1, ORDER);
        }
    }
}

/// Pull datagrams from host UDP sockets and wrap them for the guest.
#[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all)]
pub fn poll(net: &mut UserNet) {
    let keys: Vec<FlowKey> = net
        .flows
        .iter()
        .filter(|(_, f)| matches!(f, Flow::Udp(_)))
        .map(|(k, _)| *k)
        .collect();
    for key in keys {
        let mut buf = [0u8; 1500];
        let read = {
            let Some(Flow::Udp(flow)) = net.flows.get(&key) else {
                continue;
            };
            match flow.sock.recv_from(&mut buf) {
                Ok((n, _)) => Some(n),
                Err(error) if error.kind() == ErrorKind::WouldBlock => None,
                Err(error) => {
                    tracing::warn!(target: "ternvale::net::user", error = %error, reason = "udp recv", "packet drop");
                    net.stats.drops.fetch_add(1, ORDER);
                    None
                }
            }
        };
        if let Some(n) = read {
            let framed = if let Some(Flow::Udp(flow)) = net.flows.get_mut(&key) {
                flow.last = Instant::now();
                let payload = packet::udp_payload(
                    flow.remote_ip,
                    flow.guest_ip,
                    flow.remote_port,
                    flow.guest_port,
                    &buf[..n],
                );
                Some((flow.guest_mac, flow.remote_ip, flow.guest_ip, payload))
            } else {
                None
            };
            if let Some((mac, remote_ip, guest_ip, payload)) = framed {
                let frame = packet::ipv4_frame(
                    mac,
                    net.gateway_mac,
                    remote_ip,
                    guest_ip,
                    IP_PROTO_UDP,
                    net.next_ip_id(),
                    &payload,
                );
                net.stats.bytes_in.fetch_add(n as u64, ORDER);
                net.queue(frame);
            }
        }
        if let Some(Flow::Udp(flow)) = net.flows.get(&key) {
            if flow.last.elapsed() > UDP_IDLE {
                tracing::debug!(
                    target: "ternvale::net::user",
                    sport = flow.guest_port,
                    dport = flow.remote_port,
                    "closed udp flow"
                );
                net.flows.remove(&key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::handle;
    use crate::user::packet::{ipv4_frame, parse_ipv4, udp_payload, IP_PROTO_UDP};
    use crate::user::{UserNet, GUEST_IP};
    use std::net::UdpSocket;

    #[test]
    fn nats_udp_to_a_host_socket() {
        let server = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let port = server.local_addr().expect("addr").port();
        server
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .expect("to");
        let mut net = UserNet::new(Vec::new()).expect("net");
        let payload = udp_payload(GUEST_IP, [127, 0, 0, 1], 40000, port, b"ping");
        let frame = ipv4_frame(
            [0x52, 0x55, 0x0a, 0, 2, 2],
            [0x52, 0x54, 0, 0x12, 0x34, 0x56],
            GUEST_IP,
            [127, 0, 0, 1],
            IP_PROTO_UDP,
            1,
            &payload,
        );
        let ip = parse_ipv4(&frame).expect("ip");
        handle(&mut net, &ip);
        let mut buf = [0u8; 16];
        let (n, _) = server.recv_from(&mut buf).expect("recv");
        assert_eq!(&buf[..n], b"ping");
    }
}
