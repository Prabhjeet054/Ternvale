//! ICMP echo: local replies for the gateway, host datagram sockets for the rest.

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Instant;

use super::flow::{Flow, FlowKey, IcmpFlow, ICMP_IDLE};
use super::packet::{self, checksum, Ipv4, ICMP_HLEN, IP_PROTO_ICMP};
use super::{ip_str, UserNet, GATEWAY_IP, ORDER};

const ICMP_ECHO_REPLY: u8 = 0;
const ICMP_ECHO_REQUEST: u8 = 8;

/// Answer echo to the gateway locally; NAT other echo requests through ICMP datagrams.
#[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
pub fn handle(net: &mut UserNet, ip: &Ipv4<'_>) {
    if ip.payload.len() < ICMP_HLEN {
        net.drop_reason("icmp runt");
        return;
    }
    if ip.payload[0] != ICMP_ECHO_REQUEST {
        net.drop_reason("icmp not echo request");
        return;
    }
    if checksum(ip.payload) != 0 {
        net.drop_reason("icmp bad checksum");
        return;
    }
    if ip.dst == GATEWAY_IP {
        local_reply(net, ip);
        return;
    }
    nat_echo(net, ip);
}

fn local_reply(net: &mut UserNet, ip: &Ipv4<'_>) {
    let mut body = ip.payload.to_vec();
    body[0] = ICMP_ECHO_REPLY;
    body[2..4].fill(0);
    let sum = checksum(&body);
    body[2..4].copy_from_slice(&sum.to_be_bytes());
    let frame = packet::ipv4_frame(
        ip.src_mac,
        net.gateway_mac,
        GATEWAY_IP,
        ip.src,
        IP_PROTO_ICMP,
        net.next_ip_id(),
        &body,
    );
    tracing::debug!(target: "ternvale::net::user", "icmp echo reply (gateway)");
    net.queue(frame);
}

fn nat_echo(net: &mut UserNet, ip: &Ipv4<'_>) {
    if !net.icmp_ok {
        net.drop_reason("icmp datagram sockets unavailable");
        return;
    }
    let id = u16::from_be_bytes([ip.payload[4], ip.payload[5]]);
    let key = FlowKey::icmp(ip.src, ip.dst, id);
    if !net.flows.contains_key(&key) {
        if net.flows.len() >= super::MAX_FLOWS {
            net.drop_reason("flow cap");
            return;
        }
        match open_icmp() {
            Ok(fd) => {
                tracing::debug!(
                    target: "ternvale::net::user",
                    src = %ip_str(ip.src),
                    dst = %ip_str(ip.dst),
                    id,
                    "new icmp flow"
                );
                net.stats.flows.fetch_add(1, ORDER);
                net.flows.insert(
                    key,
                    Flow::Icmp(IcmpFlow {
                        fd,
                        guest_mac: ip.src_mac,
                        guest_ip: ip.src,
                        remote: ip.dst,
                        id,
                        last: Instant::now(),
                    }),
                );
            }
            Err(error) => {
                tracing::warn!(
                    target: "ternvale::net::user",
                    error = %error,
                    reason = "icmp socket",
                    "packet drop"
                );
                net.stats.drops.fetch_add(1, ORDER);
                return;
            }
        }
    }
    let Some(Flow::Icmp(flow)) = net.flows.get_mut(&key) else {
        return;
    };
    flow.last = Instant::now();
    if let Err(error) = send_echo(&flow.fd, ip.dst, ip.payload) {
        tracing::warn!(
            target: "ternvale::net::user",
            error = %error,
            reason = "icmp send",
            "packet drop"
        );
        net.stats.drops.fetch_add(1, ORDER);
    }
}

/// Recv pending ICMP replies into Ethernet frames.
#[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all)]
pub fn poll(net: &mut UserNet) {
    let keys: Vec<FlowKey> = net
        .flows
        .iter()
        .filter(|(_, f)| matches!(f, Flow::Icmp(_)))
        .map(|(k, _)| *k)
        .collect();
    for key in keys {
        let recvd = {
            let Some(Flow::Icmp(flow)) = net.flows.get(&key) else {
                continue;
            };
            recv_echo(&flow.fd)
        };
        match recvd {
            Ok(Some(body)) => {
                let (mac, guest, remote) = match net.flows.get(&key) {
                    Some(Flow::Icmp(flow)) => (flow.guest_mac, flow.guest_ip, flow.remote),
                    _ => continue,
                };
                let frame = packet::ipv4_frame(
                    mac,
                    net.gateway_mac,
                    remote,
                    guest,
                    IP_PROTO_ICMP,
                    net.next_ip_id(),
                    &body,
                );
                net.stats.bytes_in.fetch_add(body.len() as u64, ORDER);
                net.queue(frame);
                if let Some(Flow::Icmp(flow)) = net.flows.remove(&key) {
                    tracing::debug!(
                        target: "ternvale::net::user",
                        id = flow.id,
                        "closed icmp flow"
                    );
                }
            }
            Ok(None) => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => {
                tracing::warn!(
                    target: "ternvale::net::user",
                    error = %error,
                    reason = "icmp recv",
                    "packet drop"
                );
                net.stats.drops.fetch_add(1, ORDER);
            }
        }
        if let Some(Flow::Icmp(flow)) = net.flows.get_mut(&key) {
            if flow.last.elapsed() > ICMP_IDLE {
                tracing::debug!(target: "ternvale::net::user", id = flow.id, "closed icmp flow");
                net.flows.remove(&key);
            }
        }
    }
}

/// True when `SOCK_DGRAM`/`IPPROTO_ICMP` opens without EPERM.
#[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
pub fn icmp_available() -> bool {
    open_icmp().is_ok()
}

fn open_icmp() -> io::Result<OwnedFd> {
    // SAFETY: a new datagram socket; negative fd is turned into an error.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_ICMP) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a valid socket from `socket` above.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if flags >= 0 {
        // SAFETY: same live socket; O_NONBLOCK is a valid flag.
        let _rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        if _rc < 0 {
            tracing::debug!(
                target: "ternvale::net::user",
                error = %io::Error::last_os_error(),
                "icmp fcntl O_NONBLOCK"
            );
        }
    }
    // SAFETY: `fd` is a freshly opened socket we uniquely own.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn send_echo(fd: &OwnedFd, dst: [u8; 4], icmp: &[u8]) -> io::Result<()> {
    let addr = libc::sockaddr_in {
        sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from(Ipv4Addr::from(dst)).to_be(),
        },
        sin_zero: [0; 8],
    };
    // SAFETY: `addr` is a sockaddr_in; `icmp` is a live buffer.
    let n = unsafe {
        libc::sendto(
            fd.as_raw_fd(),
            icmp.as_ptr().cast(),
            icmp.len(),
            0,
            std::ptr::addr_of!(addr).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as u32,
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn recv_echo(fd: &OwnedFd) -> io::Result<Option<Vec<u8>>> {
    let mut buf = [0u8; 1500];
    // SAFETY: `buf` is writable; socket is non-blocking.
    let n = unsafe { libc::recv(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
    if n < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == ErrorKind::WouldBlock {
            return Ok(None);
        }
        return Err(err);
    }
    let n = n as usize;
    // macOS ICMP datagrams may include an IPv4 header in front of the ICMP body.
    if n >= 20 && buf[0] >> 4 == 4 {
        let ihl = usize::from(buf[0] & 0x0f) * 4;
        if ihl >= 20 && ihl < n {
            return Ok(Some(buf[ihl..n].to_vec()));
        }
    }
    Ok(Some(buf[..n].to_vec()))
}

/// Bind a TCP stream after a non-blocking `connect`.
#[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
pub fn tcp_connect_nb(addr: SocketAddrV4) -> io::Result<std::net::TcpStream> {
    // SAFETY: a new stream socket; negative fd is turned into an error.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a valid socket from `socket` above.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if flags >= 0 {
        // SAFETY: same live socket; O_NONBLOCK is a valid flag.
        let _rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        if _rc < 0 {
            tracing::debug!(
                target: "ternvale::net::user",
                error = %io::Error::last_os_error(),
                "tcp fcntl O_NONBLOCK"
            );
        }
    }
    let sin = libc::sockaddr_in {
        sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: addr.port().to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from(*addr.ip()).to_be(),
        },
        sin_zero: [0; 8],
    };
    // SAFETY: `sin` is a valid sockaddr_in for `addr`.
    let rc = unsafe {
        libc::connect(
            fd,
            std::ptr::addr_of!(sin).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as u32,
        )
    };
    if rc < 0 {
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINPROGRESS) | Some(libc::EALREADY) => {}
            _ if err.kind() == ErrorKind::WouldBlock => {}
            _ => {
                // SAFETY: `fd` is still uniquely owned and not yet wrapped.
                unsafe {
                    libc::close(fd);
                }
                return Err(err);
            }
        }
    }
    // SAFETY: unique ownership of `fd`.
    Ok(unsafe { std::net::TcpStream::from_raw_fd(fd) })
}

/// `None` = still connecting; `Some(true)` = connected; `Some(false)` = failed.
#[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all)]
pub fn tcp_connect_ready(sock: &std::net::TcpStream) -> io::Result<Option<bool>> {
    let mut pfd = libc::pollfd {
        fd: sock.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: `pfd` is one valid pollfd; timeout 0 does not block.
    let n = unsafe { libc::poll(std::ptr::addr_of_mut!(pfd), 1, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    if n == 0 {
        return Ok(None);
    }
    match sock.take_error()? {
        Some(error)
            if error.kind() == ErrorKind::WouldBlock
                || error.raw_os_error() == Some(libc::EINPROGRESS)
                || error.raw_os_error() == Some(libc::EALREADY) =>
        {
            Ok(None)
        }
        Some(error) => Err(error),
        None => {
            if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                Ok(Some(false))
            } else if pfd.revents & libc::POLLOUT != 0 {
                Ok(Some(true))
            } else {
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::icmp_available;

    #[test]
    fn icmp_datagram_socket_opens_or_is_documented() {
        let ok = icmp_available();
        tracing::info!(
            target: "ternvale::net::user",
            available = ok,
            "unprivileged ICMP SOCK_DGRAM"
        );
        // macOS allows this for echo; if a host denies it, NAT still does TCP/UDP/DHCP/DNS.
        let _ = ok;
    }

    #[test]
    fn gateway_echo_replies_locally() {
        use crate::user::packet::{checksum, ipv4_frame, parse_ipv4, ICMP_HLEN, IP_PROTO_ICMP};
        use crate::user::{UserNet, GATEWAY_IP, GUEST_IP};
        let mut body = vec![0u8; ICMP_HLEN];
        body[0] = 8;
        body[4..8].copy_from_slice(&[0, 1, 0, 1]);
        let sum = checksum(&body);
        body[2..4].copy_from_slice(&sum.to_be_bytes());
        let frame = ipv4_frame(
            [0x52, 0x55, 0x0a, 0, 2, 2],
            [0x52, 0x54, 0, 0x12, 0x34, 0x56],
            GUEST_IP,
            GATEWAY_IP,
            IP_PROTO_ICMP,
            1,
            &body,
        );
        let mut net = UserNet::new(Vec::new()).expect("net");
        super::handle(&mut net, &parse_ipv4(&frame).expect("ip"));
        let reply = net.poll().expect("echo reply");
        let ip = parse_ipv4(&reply).expect("ip");
        assert_eq!(ip.src, GATEWAY_IP);
        assert_eq!(ip.payload[0], 0);
    }
}
