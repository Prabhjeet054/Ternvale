//! Connection tracking: keys, idle timeouts, and the flow table cap.

use std::net::{TcpListener, TcpStream, UdpSocket};
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

/// Drop a TCP flow after this idle time.
pub const TCP_IDLE: Duration = Duration::from_secs(300);
/// Drop a TCP flow that is already closing.
pub const TCP_CLOSING: Duration = Duration::from_secs(2);
/// Drop a UDP flow after this idle time.
pub const UDP_IDLE: Duration = Duration::from_secs(60);
/// Drop an ICMP flow after this idle time.
pub const ICMP_IDLE: Duration = Duration::from_secs(10);

/// 5-tuple (ICMP uses `id` in `guest_port`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FlowKey {
    pub proto: u8,
    pub guest_ip: [u8; 4],
    pub guest_port: u16,
    pub remote_ip: [u8; 4],
    pub remote_port: u16,
}

impl FlowKey {
    pub fn udp(guest_ip: [u8; 4], guest_port: u16, remote_ip: [u8; 4], remote_port: u16) -> Self {
        Self {
            proto: super::packet::IP_PROTO_UDP,
            guest_ip,
            guest_port,
            remote_ip,
            remote_port,
        }
    }

    pub fn tcp(guest_ip: [u8; 4], guest_port: u16, remote_ip: [u8; 4], remote_port: u16) -> Self {
        Self {
            proto: super::packet::IP_PROTO_TCP,
            guest_ip,
            guest_port,
            remote_ip,
            remote_port,
        }
    }

    pub fn icmp(guest_ip: [u8; 4], remote_ip: [u8; 4], id: u16) -> Self {
        Self {
            proto: super::packet::IP_PROTO_ICMP,
            guest_ip,
            guest_port: id,
            remote_ip,
            remote_port: 0,
        }
    }
}

/// One tracked NAT flow.
pub enum Flow {
    Udp(UdpFlow),
    Tcp(TcpFlow),
    Icmp(IcmpFlow),
}

/// UDP NAT mapping onto a host datagram socket.
pub struct UdpFlow {
    pub sock: UdpSocket,
    pub guest_mac: [u8; 6],
    pub guest_ip: [u8; 4],
    pub guest_port: u16,
    pub remote_ip: [u8; 4],
    pub remote_port: u16,
    pub last: Instant,
}

/// TCP proxy onto a host stream socket.
pub struct TcpFlow {
    pub sock: TcpStream,
    pub guest_mac: [u8; 6],
    pub guest_ip: [u8; 4],
    pub guest_port: u16,
    pub remote_ip: [u8; 4],
    pub remote_port: u16,
    pub inbound: bool,
    pub state: TcpState,
    pub guest_seq: u32,
    pub our_seq: u32,
    pub last: Instant,
}

/// ICMP echo mapping onto an unprivileged datagram socket.
pub struct IcmpFlow {
    pub fd: OwnedFd,
    pub guest_mac: [u8; 6],
    pub guest_ip: [u8; 4],
    pub remote: [u8; 4],
    pub id: u16,
    pub last: Instant,
}

/// Host listener that injects a SYN toward the guest.
pub struct Listener {
    pub tcp: Option<TcpListener>,
    pub udp: Option<UdpSocket>,
    pub guest_port: u16,
}

/// TCP handshake / established / closing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    /// Host connect in progress (guest SYN already seen).
    Connecting,
    /// SYN-ACK sent, waiting for guest ACK.
    SynReceived,
    /// Data can flow.
    Established,
    /// Guest sent FIN.
    CloseWait,
    /// We sent FIN.
    LastAck,
}

#[cfg(test)]
mod tests {
    use super::{FlowKey, TCP_IDLE, UDP_IDLE};

    #[test]
    fn keys_differ_by_proto() {
        let u = FlowKey::udp([10, 0, 2, 15], 1, [1, 1, 1, 1], 53);
        let t = FlowKey::tcp([10, 0, 2, 15], 1, [1, 1, 1, 1], 53);
        assert_ne!(u, t);
        assert!(TCP_IDLE > UDP_IDLE);
    }
}
