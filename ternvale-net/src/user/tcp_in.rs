//! Inbound TCP from a host port-forward listener.

use std::time::Instant;

use super::flow::{Flow, FlowKey, TcpFlow, TcpState};
use super::packet::{self, IP_PROTO_TCP, TCP_SYN};
use super::tcp::WINDOW;
use super::{UserNet, GATEWAY_IP, ORDER};

/// Inject a SYN toward the guest for an accepted host connection.
#[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
pub fn inbound(net: &mut UserNet, sock: std::net::TcpStream, guest_port: u16) {
    if net.flows.len() >= super::MAX_FLOWS {
        net.drop_reason("flow cap");
        return;
    }
    let Ok(peer) = sock.peer_addr() else {
        net.drop_reason("tcp inbound peer");
        return;
    };
    if !peer.is_ipv4() {
        net.drop_reason("tcp inbound not ipv4");
        return;
    }
    let guest_port_ephemeral = net.next_host_port();
    let key = FlowKey::tcp(
        super::GUEST_IP,
        guest_port,
        GATEWAY_IP,
        guest_port_ephemeral,
    );
    let our_seq = net.next_isn();
    if let Err(error) = sock.set_nonblocking(true) {
        tracing::debug!(
            target: "ternvale::net::user",
            error = %error,
            "tcp inbound nonblocking"
        );
    }
    tracing::debug!(
        target: "ternvale::net::user",
        host = %peer,
        guest_port,
        "new inbound tcp flow"
    );
    net.stats.flows.fetch_add(1, ORDER);
    net.flows.insert(
        key,
        Flow::Tcp(TcpFlow {
            sock,
            guest_mac: net.guest_mac,
            guest_ip: super::GUEST_IP,
            guest_port,
            remote_ip: GATEWAY_IP,
            remote_port: guest_port_ephemeral,
            inbound: true,
            state: TcpState::SynReceived,
            guest_seq: 0,
            our_seq,
            last: Instant::now(),
        }),
    );
    let syn = packet::tcp_payload(
        GATEWAY_IP,
        super::GUEST_IP,
        guest_port_ephemeral,
        guest_port,
        our_seq,
        0,
        TCP_SYN,
        WINDOW,
        &[],
    );
    let frame = packet::ipv4_frame(
        net.guest_mac,
        net.gateway_mac,
        GATEWAY_IP,
        super::GUEST_IP,
        IP_PROTO_TCP,
        net.next_ip_id(),
        &syn,
    );
    net.queue(frame);
    if let Some(Flow::Tcp(flow)) = net.flows.get_mut(&key) {
        flow.our_seq = flow.our_seq.wrapping_add(1);
    }
}
