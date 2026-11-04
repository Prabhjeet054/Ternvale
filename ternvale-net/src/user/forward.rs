//! Host listeners for `forwards = [{host_port, guest_port, proto}]`.

use std::io::ErrorKind;
use std::net::{SocketAddrV4, TcpListener, UdpSocket};

use super::flow::Listener;
use super::packet::{self, IP_PROTO_UDP};
use super::{Forward, Proto, UserError, UserNet, GUEST_IP, ORDER};

/// Bind one host port per forward rule.
#[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
pub fn bind_all(forwards: &[Forward]) -> Result<Vec<Listener>, UserError> {
    let mut out = Vec::with_capacity(forwards.len());
    for spec in forwards {
        let addr = SocketAddrV4::new(spec.host, spec.host_port);
        match spec.proto {
            Proto::Tcp => {
                let tcp = TcpListener::bind(addr).map_err(|source| UserError::Bind {
                    addr: addr.to_string(),
                    source,
                })?;
                tcp.set_nonblocking(true)
                    .map_err(|source| UserError::Bind {
                        addr: addr.to_string(),
                        source,
                    })?;
                tracing::info!(
                    target: "ternvale::net::user",
                    proto = "tcp",
                    host = %addr,
                    guest_port = spec.guest_port,
                    "port forward listening"
                );
                out.push(Listener {
                    tcp: Some(tcp),
                    udp: None,
                    guest_port: spec.guest_port,
                });
            }
            Proto::Udp => {
                let udp = UdpSocket::bind(addr).map_err(|source| UserError::Bind {
                    addr: addr.to_string(),
                    source,
                })?;
                udp.set_nonblocking(true)
                    .map_err(|source| UserError::Bind {
                        addr: addr.to_string(),
                        source,
                    })?;
                tracing::info!(
                    target: "ternvale::net::user",
                    proto = "udp",
                    host = %addr,
                    guest_port = spec.guest_port,
                    "port forward listening"
                );
                out.push(Listener {
                    tcp: None,
                    udp: Some(udp),
                    guest_port: spec.guest_port,
                });
            }
        }
    }
    Ok(out)
}

/// Accept TCP and inject SYNs; rewrite inbound UDP toward the guest.
#[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all)]
pub fn poll(net: &mut UserNet) {
    for i in 0..net.listeners.len() {
        let guest_port = net.listeners[i].guest_port;
        loop {
            let accepted = net.listeners[i]
                .tcp
                .as_ref()
                .and_then(|tcp| tcp.accept().ok());
            let Some((sock, peer)) = accepted else {
                break;
            };
            tracing::debug!(
                target: "ternvale::net::user",
                peer = %peer,
                guest_port,
                "accepted forwarded tcp"
            );
            if let Err(error) = sock.set_nonblocking(true) {
                tracing::debug!(
                    target: "ternvale::net::user",
                    error = %error,
                    "forwarded tcp nonblocking"
                );
            }
            super::tcp_in::inbound(net, sock, guest_port);
        }
        let mut buf = [0u8; 1500];
        let recvd = net.listeners[i]
            .udp
            .as_ref()
            .map(|udp| udp.recv_from(&mut buf));
        match recvd {
            Some(Ok((n, from))) => {
                let std::net::SocketAddr::V4(v4) = from else {
                    net.drop_reason("udp forward not ipv4");
                    continue;
                };
                let src = v4.ip().octets();
                let payload = packet::udp_payload(src, GUEST_IP, v4.port(), guest_port, &buf[..n]);
                let frame = packet::ipv4_frame(
                    net.guest_mac,
                    net.gateway_mac,
                    src,
                    GUEST_IP,
                    IP_PROTO_UDP,
                    net.next_ip_id(),
                    &payload,
                );
                net.stats.bytes_in.fetch_add(n as u64, ORDER);
                net.queue(frame);
            }
            Some(Err(error)) if error.kind() == ErrorKind::WouldBlock => {}
            Some(Err(error)) => {
                tracing::warn!(
                    target: "ternvale::net::user",
                    error = %error,
                    reason = "udp forward recv",
                    "packet drop"
                );
                net.stats.drops.fetch_add(1, ORDER);
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::bind_all;
    use crate::user::{Forward, Proto};

    #[test]
    fn binds_tcp_localhost() {
        let spec = Forward {
            proto: Proto::Tcp,
            host: std::net::Ipv4Addr::LOCALHOST,
            host_port: 0,
            guest_port: 22,
        };
        let listeners = bind_all(&[spec]).expect("bind");
        assert_eq!(listeners.len(), 1);
        assert!(listeners[0].tcp.is_some());
    }
}
