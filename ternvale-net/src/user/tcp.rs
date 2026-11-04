//! TCP proxy: terminate the guest handshake and splice onto a host `TcpStream`.

use std::io::{ErrorKind, Read, Write};
use std::net::SocketAddrV4;
use std::time::Instant;

use super::flow::{Flow, FlowKey, TcpFlow, TcpState, TCP_CLOSING, TCP_IDLE};
use super::icmp::{tcp_connect_nb, tcp_connect_ready};
use super::packet::{
    self, parse_tcp, Ipv4, TcpHdr, IP_PROTO_TCP, TCP_ACK, TCP_FIN, TCP_PSH, TCP_RST, TCP_SYN,
};
use super::{ip_str, UserNet, ORDER};

pub(crate) const WINDOW: u16 = 65535;

/// Guest-initiated TCP (SYN / data / FIN / RST).
#[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
pub fn handle(net: &mut UserNet, ip: &Ipv4<'_>) {
    let Some(tcp) = parse_tcp(ip.payload) else {
        net.drop_reason("tcp runt");
        return;
    };
    let key = FlowKey::tcp(ip.src, tcp.src_port, ip.dst, tcp.dst_port);
    if tcp.flags & TCP_SYN != 0 && tcp.flags & TCP_ACK == 0 {
        on_syn(net, ip, &tcp, key);
        return;
    }
    if tcp.flags & TCP_RST != 0 {
        if net.flows.remove(&key).is_some() {
            tracing::debug!(target: "ternvale::net::user", dport = tcp.dst_port, "closed tcp flow (rst)");
        }
        return;
    }
    let inbound = match net.flows.get(&key) {
        Some(Flow::Tcp(flow)) => flow.inbound,
        _ => {
            if tcp.flags == TCP_ACK && tcp.data.is_empty() {
                return;
            }
            net.drop_reason("tcp no flow");
            return;
        }
    };
    if inbound && tcp.flags & TCP_SYN != 0 && tcp.flags & TCP_ACK != 0 {
        if let Some(Flow::Tcp(flow)) = net.flows.get_mut(&key) {
            flow.guest_seq = tcp.seq.wrapping_add(1);
            flow.state = TcpState::Established;
            flow.last = Instant::now();
        }
        ack_guest(net, key);
        return;
    }
    let write_err = {
        let Some(Flow::Tcp(flow)) = net.flows.get_mut(&key) else {
            return;
        };
        flow.last = Instant::now();
        if matches!(flow.state, TcpState::SynReceived) && tcp.flags & TCP_ACK != 0 {
            flow.state = TcpState::Established;
            tracing::debug!(target: "ternvale::net::user", dport = tcp.dst_port, "tcp established");
        }
        if tcp.data.is_empty() {
            None
        } else {
            match flow.sock.write_all(tcp.data) {
                Ok(()) => {
                    flow.guest_seq = flow.guest_seq.wrapping_add(tcp.data.len() as u32);
                    net.stats.bytes_out.fetch_add(tcp.data.len() as u64, ORDER);
                    None
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => Some(None),
                Err(error) => Some(Some(error)),
            }
        }
    };
    match write_err {
        Some(None) => {
            net.drop_reason("tcp write would block");
        }
        Some(Some(error)) => {
            tracing::warn!(target: "ternvale::net::user", error = %error, reason = "tcp write", "packet drop");
            net.stats.drops.fetch_add(1, ORDER);
            send_rst(net, key);
            return;
        }
        None => {}
    }
    if tcp.flags & TCP_FIN != 0 {
        if let Some(Flow::Tcp(flow)) = net.flows.get_mut(&key) {
            flow.guest_seq = flow.guest_seq.wrapping_add(1);
            if let Err(error) = flow.sock.shutdown(std::net::Shutdown::Write) {
                tracing::debug!(
                    target: "ternvale::net::user",
                    error = %error,
                    "tcp shutdown write after guest fin"
                );
            }
            flow.state = TcpState::CloseWait;
        }
    }
    let closing = matches!(
        net.flows.get(&key),
        Some(Flow::Tcp(flow)) if flow.state == TcpState::LastAck
    );
    if closing && tcp.flags & TCP_ACK != 0 && tcp.flags & TCP_FIN == 0 {
        tracing::debug!(target: "ternvale::net::user", dport = tcp.dst_port, "closed tcp flow");
        net.flows.remove(&key);
        return;
    }
    ack_guest(net, key);
}

fn on_syn(net: &mut UserNet, ip: &Ipv4<'_>, tcp: &TcpHdr<'_>, key: FlowKey) {
    if net.flows.contains_key(&key) {
        return;
    }
    if net.flows.len() >= super::MAX_FLOWS {
        net.drop_reason("flow cap");
        return;
    }
    let addr = SocketAddrV4::new(UserNet::nat_v4(ip.dst), tcp.dst_port);
    let sock = match tcp_connect_nb(addr) {
        Ok(sock) => sock,
        Err(error) => {
            tracing::warn!(target: "ternvale::net::user", error = %error, reason = "tcp connect", "packet drop");
            net.stats.drops.fetch_add(1, ORDER);
            return;
        }
    };
    tracing::debug!(
        target: "ternvale::net::user",
        src = %ip_str(ip.src),
        sport = tcp.src_port,
        dst = %ip_str(ip.dst),
        dport = tcp.dst_port,
        "new tcp flow"
    );
    net.stats.flows.fetch_add(1, ORDER);
    let our_seq = net.next_isn();
    net.flows.insert(
        key,
        Flow::Tcp(TcpFlow {
            sock,
            guest_mac: ip.src_mac,
            guest_ip: ip.src,
            guest_port: tcp.src_port,
            remote_ip: ip.dst,
            remote_port: tcp.dst_port,
            inbound: false,
            state: TcpState::Connecting,
            guest_seq: tcp.seq.wrapping_add(1),
            our_seq,
            last: Instant::now(),
        }),
    );
}

/// Host-to-guest reads, connecting sockets, idle expiry.
#[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all)]
pub fn poll(net: &mut UserNet) {
    let keys: Vec<FlowKey> = net
        .flows
        .iter()
        .filter(|(_, f)| matches!(f, Flow::Tcp(_)))
        .map(|(k, _)| *k)
        .collect();
    for key in keys {
        complete_connect(net, key);
        splice_in(net, key);
        expire(net, key);
    }
}

fn complete_connect(net: &mut UserNet, key: FlowKey) {
    let ready = {
        let Some(Flow::Tcp(flow)) = net.flows.get_mut(&key) else {
            return;
        };
        if flow.state != TcpState::Connecting {
            return;
        }
        match tcp_connect_ready(&flow.sock) {
            Ok(Some(true)) => Some(true),
            Ok(Some(false)) => {
                tracing::warn!(
                    target: "ternvale::net::user",
                    reason = "tcp connect failed",
                    "packet drop"
                );
                net.stats.drops.fetch_add(1, ORDER);
                Some(false)
            }
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(target: "ternvale::net::user", error = %error, reason = "tcp connect failed", "packet drop");
                net.stats.drops.fetch_add(1, ORDER);
                Some(false)
            }
        }
    };
    if ready == Some(false) {
        net.flows.remove(&key);
        return;
    }
    if ready != Some(true) {
        return;
    }
    let (syn_ack, mac, remote_ip, guest_ip) = {
        let Some(Flow::Tcp(flow)) = net.flows.get_mut(&key) else {
            return;
        };
        flow.state = TcpState::SynReceived;
        let syn_ack = packet::tcp_payload(
            flow.remote_ip,
            flow.guest_ip,
            flow.remote_port,
            flow.guest_port,
            flow.our_seq,
            flow.guest_seq,
            TCP_SYN | TCP_ACK,
            WINDOW,
            &[],
        );
        let mac = flow.guest_mac;
        let remote_ip = flow.remote_ip;
        let guest_ip = flow.guest_ip;
        flow.our_seq = flow.our_seq.wrapping_add(1);
        (syn_ack, mac, remote_ip, guest_ip)
    };
    let gw = net.gateway_mac;
    let id = net.next_ip_id();
    let frame = packet::ipv4_frame(mac, gw, remote_ip, guest_ip, IP_PROTO_TCP, id, &syn_ack);
    net.queue(frame);
}

fn splice_in(net: &mut UserNet, key: FlowKey) {
    let mut buf = [0u8; 1400];
    let n = {
        let Some(Flow::Tcp(flow)) = net.flows.get_mut(&key) else {
            return;
        };
        if !matches!(flow.state, TcpState::Established | TcpState::CloseWait) {
            return;
        }
        match flow.sock.read(&mut buf) {
            Ok(n) => n,
            Err(error) if error.kind() == ErrorKind::WouldBlock => return,
            Err(error) => {
                tracing::warn!(target: "ternvale::net::user", error = %error, reason = "tcp read", "packet drop");
                net.stats.drops.fetch_add(1, ORDER);
                return;
            }
        }
    };
    let (payload, mac, remote_ip, guest_ip) = {
        let Some(Flow::Tcp(flow)) = net.flows.get_mut(&key) else {
            return;
        };
        let flags = if n == 0 {
            flow.state = TcpState::LastAck;
            TCP_FIN | TCP_ACK
        } else {
            TCP_PSH | TCP_ACK
        };
        let payload = packet::tcp_payload(
            flow.remote_ip,
            flow.guest_ip,
            flow.remote_port,
            flow.guest_port,
            flow.our_seq,
            flow.guest_seq,
            flags,
            WINDOW,
            &buf[..n],
        );
        let mac = flow.guest_mac;
        let remote_ip = flow.remote_ip;
        let guest_ip = flow.guest_ip;
        flow.our_seq = flow.our_seq.wrapping_add(n.max(1) as u32);
        (payload, mac, remote_ip, guest_ip)
    };
    if n > 0 {
        net.stats.bytes_in.fetch_add(n as u64, ORDER);
    }
    let gw = net.gateway_mac;
    let id = net.next_ip_id();
    net.queue(packet::ipv4_frame(
        mac,
        gw,
        remote_ip,
        guest_ip,
        IP_PROTO_TCP,
        id,
        &payload,
    ));
}

fn ack_guest(net: &mut UserNet, key: FlowKey) {
    let copied = net.flows.get(&key).and_then(|flow| match flow {
        Flow::Tcp(flow) => Some((
            packet::tcp_payload(
                flow.remote_ip,
                flow.guest_ip,
                flow.remote_port,
                flow.guest_port,
                flow.our_seq,
                flow.guest_seq,
                TCP_ACK,
                WINDOW,
                &[],
            ),
            flow.guest_mac,
            flow.remote_ip,
            flow.guest_ip,
        )),
        _ => None,
    });
    let Some((ack, mac, remote_ip, guest_ip)) = copied else {
        return;
    };
    let gw = net.gateway_mac;
    let id = net.next_ip_id();
    net.queue(packet::ipv4_frame(
        mac,
        gw,
        remote_ip,
        guest_ip,
        IP_PROTO_TCP,
        id,
        &ack,
    ));
}

fn send_rst(net: &mut UserNet, key: FlowKey) {
    if let Some(Flow::Tcp(flow)) = net.flows.remove(&key) {
        let rst = packet::tcp_payload(
            flow.remote_ip,
            flow.guest_ip,
            flow.remote_port,
            flow.guest_port,
            flow.our_seq,
            flow.guest_seq,
            TCP_RST | TCP_ACK,
            0,
            &[],
        );
        let frame = packet::ipv4_frame(
            flow.guest_mac,
            net.gateway_mac,
            flow.remote_ip,
            flow.guest_ip,
            IP_PROTO_TCP,
            net.next_ip_id(),
            &rst,
        );
        tracing::debug!(target: "ternvale::net::user", dport = flow.remote_port, "closed tcp flow (rst)");
        net.queue(frame);
    }
}

fn expire(net: &mut UserNet, key: FlowKey) {
    let idle = match net.flows.get(&key) {
        Some(Flow::Tcp(flow)) => {
            let limit = match flow.state {
                TcpState::CloseWait | TcpState::LastAck => TCP_CLOSING,
                _ => TCP_IDLE,
            };
            flow.last.elapsed() > limit
        }
        _ => false,
    };
    if idle {
        tracing::debug!(target: "ternvale::net::user", "closed tcp flow (idle)");
        net.flows.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::handle;
    use crate::user::packet::{ipv4_frame, parse_ipv4, tcp_payload, IP_PROTO_TCP, TCP_SYN};
    use crate::user::{UserNet, GATEWAY_MAC, GUEST_IP};
    use std::net::TcpListener;

    #[test]
    fn syn_opens_a_host_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        listener.set_nonblocking(true).expect("nb");
        let mut net = UserNet::new(Vec::new()).expect("net");
        let tcp = tcp_payload(
            GUEST_IP,
            [127, 0, 0, 1],
            41000,
            port,
            1000,
            0,
            TCP_SYN,
            65535,
            &[],
        );
        let frame = ipv4_frame(
            GATEWAY_MAC,
            [0x52, 0x54, 0, 0x12, 0x34, 0x56],
            GUEST_IP,
            [127, 0, 0, 1],
            IP_PROTO_TCP,
            1,
            &tcp,
        );
        handle(&mut net, &parse_ipv4(&frame).expect("ip"));
        assert_eq!(net.flow_count(), 1);
    }

    #[test]
    fn syn_to_gateway_connects_localhost() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        listener.set_nonblocking(true).expect("nb");
        let mut net = UserNet::new(Vec::new()).expect("net");
        let tcp = tcp_payload(
            GUEST_IP,
            crate::user::GATEWAY_IP,
            41001,
            port,
            1000,
            0,
            TCP_SYN,
            65535,
            &[],
        );
        let frame = ipv4_frame(
            GATEWAY_MAC,
            [0x52, 0x54, 0, 0x12, 0x34, 0x56],
            GUEST_IP,
            crate::user::GATEWAY_IP,
            IP_PROTO_TCP,
            1,
            &tcp,
        );
        handle(&mut net, &parse_ipv4(&frame).expect("ip"));
        assert_eq!(net.flow_count(), 1);
    }
}
