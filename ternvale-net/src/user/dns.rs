//! DNS forwarder on [`crate::user::DNS_IP`] / the gateway, using host resolvers.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use super::packet::{self, parse_udp, Ipv4};
use super::{ip_str, UserNet, DNS_IP, DNS_PORT, GATEWAY_IP};

/// Forward a guest DNS query to the first `nameserver` in `/etc/resolv.conf`.
#[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
pub fn handle(net: &mut UserNet, ip: &Ipv4<'_>) {
    let Some(udp) = parse_udp(ip.payload) else {
        net.drop_reason("dns runt");
        return;
    };
    if udp.dst_port != DNS_PORT || udp.data.len() < 12 {
        net.drop_reason("dns not a query");
        return;
    }
    let Some(resolver) = host_resolver() else {
        net.drop_reason("dns no host resolver");
        return;
    };
    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(sock) => sock,
        Err(error) => {
            tracing::warn!(
                target: "ternvale::net::user",
                error = %error,
                reason = "dns bind",
                "packet drop"
            );
            net.stats.drops.fetch_add(1, super::ORDER);
            return;
        }
    };
    if let Err(error) = sock.set_read_timeout(Some(Duration::from_millis(800))) {
        tracing::warn!(target: "ternvale::net::user", error = %error, reason = "dns timeout", "packet drop");
        net.stats.drops.fetch_add(1, super::ORDER);
        return;
    }
    if let Err(error) = sock.send_to(udp.data, resolver) {
        tracing::warn!(target: "ternvale::net::user", error = %error, reason = "dns send", "packet drop");
        net.stats.drops.fetch_add(1, super::ORDER);
        return;
    }
    let mut buf = [0u8; 1500];
    let (n, _) = match sock.recv_from(&mut buf) {
        Ok(got) => got,
        Err(error) => {
            tracing::warn!(
                target: "ternvale::net::user",
                error = %error,
                reason = "dns recv",
                "packet drop"
            );
            net.stats.drops.fetch_add(1, super::ORDER);
            return;
        }
    };
    let server = if ip.dst == DNS_IP { DNS_IP } else { GATEWAY_IP };
    let payload = packet::udp_payload(server, ip.src, DNS_PORT, udp.src_port, &buf[..n]);
    let frame = packet::ipv4_frame(
        ip.src_mac,
        net.gateway_mac,
        server,
        ip.src,
        packet::IP_PROTO_UDP,
        net.next_ip_id(),
        &payload,
    );
    tracing::debug!(
        target: "ternvale::net::user",
        resolver = %resolver,
        bytes = n,
        "dns forwarded"
    );
    net.stats.bytes_out.fetch_add(n as u64, super::ORDER);
    net.queue(frame);
}

#[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
fn host_resolver() -> Option<SocketAddr> {
    let text = std::fs::read_to_string("/etc/resolv.conf").ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("nameserver") {
            let addr = rest.split_whitespace().next()?;
            if let Ok(ip) = addr.parse::<std::net::Ipv4Addr>() {
                tracing::debug!(target: "ternvale::net::user", resolver = %ip_str(ip.octets()), "host resolver");
                return Some(SocketAddr::from((ip, DNS_PORT)));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::host_resolver;

    #[test]
    fn resolv_conf_has_or_lacks_a_nameserver() {
        let _ = host_resolver();
    }
}
