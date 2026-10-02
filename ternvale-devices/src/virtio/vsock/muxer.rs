//! Connection table: guest packets in, host Unix streams polled, packets out.
//!
//! Connections are keyed by `(host_port, guest_port)`; the CIDs are fixed per
//! device. Packets with no connection payload (RESPONSE, REQUEST, RST) go on a
//! control queue that is drained before data.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::conn::{Conn, State};
use super::host::HostSide;
use super::packet::{
    Header, OP_CREDIT_REQUEST, OP_CREDIT_UPDATE, OP_REQUEST, OP_RESPONSE, OP_RST, OP_RW,
    OP_SHUTDOWN, SHUTDOWN_BOTH, TYPE_STREAM,
};
use super::{VsockStats, HOST_CID};

/// How long a host-initiated REQUEST or a sent SHUTDOWN waits before RST.
// TODO(verify): Linux answers a full SHUTDOWN with RST only once its socket has
// no unread data; an application that never reads will hit this timeout.
pub(super) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// First local port for host-initiated connections, above common service ports.
const EPHEMERAL_BASE: u32 = 0x8000_0000;

type Key = (u32, u32);

pub(super) struct Muxer {
    guest_cid: u64,
    host: HostSide,
    conns: BTreeMap<Key, Conn>,
    control: VecDeque<Header>,
    cursor: Option<Key>,
    next_port: u32,
    stats: Arc<VsockStats>,
}

impl Muxer {
    pub(super) fn new(guest_cid: u32, host: HostSide, stats: Arc<VsockStats>) -> Self {
        Self {
            guest_cid: u64::from(guest_cid),
            host,
            conns: BTreeMap::new(),
            control: VecDeque::new(),
            cursor: None,
            next_port: EPHEMERAL_BASE,
            stats,
        }
    }

    pub(super) fn connections(&self) -> usize {
        self.conns.len()
    }

    /// Drop every connection (device reset). Host streams close.
    pub(super) fn reset(&mut self) {
        let dropped = self.conns.len();
        self.conns.clear();
        self.control.clear();
        tracing::debug!(target: "ternvale::virtio::vsock", dropped, "vsock connections reset");
    }

    /// One packet from the TX queue.
    pub(super) fn handle_guest(&mut self, hdr: &Header, payload: &[u8]) {
        if hdr.src_cid != self.guest_cid || hdr.dst_cid != HOST_CID {
            tracing::warn!(
                target: "ternvale::virtio::vsock",
                src_cid = hdr.src_cid,
                dst_cid = hdr.dst_cid,
                guest_cid = self.guest_cid,
                "vsock packet with wrong cid dropped"
            );
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if hdr.kind != TYPE_STREAM {
            tracing::warn!(
                target: "ternvale::virtio::vsock",
                kind = hdr.kind,
                "unsupported vsock socket type; resetting"
            );
            self.reply_rst(hdr);
            return;
        }
        let key = (hdr.dst_port, hdr.src_port);
        match hdr.op {
            OP_REQUEST => return self.guest_connect(key, hdr),
            OP_RST => {
                if self.conns.remove(&key).is_some() {
                    self.closed(key, "guest reset");
                }
                return;
            }
            _ => {}
        }
        let Some(conn) = self.conns.get_mut(&key) else {
            tracing::debug!(
                target: "ternvale::virtio::vsock",
                host_port = key.0,
                guest_port = key.1,
                "vsock packet for unknown connection; resetting"
            );
            self.reply_rst(hdr);
            return;
        };
        conn.update_peer(hdr.buf_alloc, hdr.fwd_cnt);
        let failure = match hdr.op {
            OP_RESPONSE if matches!(conn.state, State::Connecting(_)) => {
                conn.state = State::Established;
                tracing::info!(
                    target: "ternvale::virtio::vsock",
                    host_port = key.0,
                    guest_port = key.1,
                    "vsock host connection established"
                );
                None
            }
            OP_RESPONSE => Some("unexpected response"),
            OP_RW if conn.state != State::Established => Some("data before established"),
            OP_RW if !conn.accept_payload(payload) => Some("guest exceeded its credit"),
            OP_RW => conn.flush().err().map(|_| "host write failed"),
            OP_CREDIT_UPDATE => None,
            OP_CREDIT_REQUEST => {
                conn.request_credit_update();
                None
            }
            OP_SHUTDOWN => {
                conn.peer_shutdown |= hdr.flags & SHUTDOWN_BOTH;
                tracing::debug!(
                    target: "ternvale::virtio::vsock",
                    host_port = key.0,
                    guest_port = key.1,
                    flags = conn.peer_shutdown,
                    "vsock guest shutdown"
                );
                conn.flush().err().map(|_| "host write failed")
            }
            _ => Some("unknown op"),
        };
        if let Some(reason) = failure {
            tracing::warn!(
                target: "ternvale::virtio::vsock",
                host_port = key.0,
                guest_port = key.1,
                reason,
                "vsock protocol error; resetting"
            );
            self.close(key, reason);
            return;
        }
        self.finish_if_shut(key);
    }

    fn guest_connect(&mut self, key: Key, hdr: &Header) {
        if self.conns.contains_key(&key) {
            tracing::warn!(
                target: "ternvale::virtio::vsock",
                host_port = key.0,
                guest_port = key.1,
                "duplicate vsock request; resetting"
            );
            self.close(key, "duplicate request");
            return;
        }
        let stream = match self.host.connect(key.0) {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(
                    target: "ternvale::virtio::vsock",
                    port = key.0,
                    path = %self.host.host_path(key.0).display(),
                    error = %error,
                    "no host listener for vsock port; resetting"
                );
                self.reply_rst(hdr);
                return;
            }
        };
        let mut conn = Conn::new(stream, State::Established);
        conn.update_peer(hdr.buf_alloc, hdr.fwd_cnt);
        let response = self.conn_header(key, &mut conn, OP_RESPONSE, 0, 0);
        self.conns.insert(key, conn);
        self.control.push_back(response);
        self.stats.connections.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            target: "ternvale::virtio::vsock",
            host_port = key.0,
            guest_port = key.1,
            "vsock guest connection accepted"
        );
    }

    /// Accept host connections, move bytes both ways, and expire handshakes.
    pub(super) fn poll_host(&mut self, now: Instant) {
        for (guest_port, stream) in self.host.accept() {
            let key = self.alloc_key(guest_port);
            let mut conn = Conn::new(stream, State::Connecting(now));
            let request = self.conn_header(key, &mut conn, OP_REQUEST, 0, 0);
            self.conns.insert(key, conn);
            self.control.push_back(request);
            self.stats.connections.fetch_add(1, Ordering::Relaxed);
            tracing::info!(
                target: "ternvale::virtio::vsock",
                host_port = key.0,
                guest_port,
                "vsock host connection requested"
            );
        }
        let keys: Vec<Key> = self.conns.keys().copied().collect();
        for key in keys {
            let Some(conn) = self.conns.get_mut(&key) else {
                continue;
            };
            let failure = match conn.state {
                State::Connecting(since) if now.duration_since(since) > HANDSHAKE_TIMEOUT => {
                    Some("guest did not answer the request")
                }
                State::Closing(since) if now.duration_since(since) > HANDSHAKE_TIMEOUT => {
                    Some("guest did not reset after shutdown")
                }
                _ => match conn.flush() {
                    Err(_) => Some("host write failed"),
                    Ok(_) => conn.fill().err().map(|_| "host read failed"),
                },
            };
            match failure {
                Some(reason) => {
                    tracing::debug!(
                        target: "ternvale::virtio::vsock",
                        host_port = key.0,
                        guest_port = key.1,
                        reason,
                        "vsock connection aborted"
                    );
                    self.close(key, reason);
                }
                None => self.finish_if_shut(key),
            }
        }
    }

    pub(super) fn has_rx(&self) -> bool {
        !self.control.is_empty() || self.conns.values().any(Conn::wants_rx)
    }

    /// Next packet for an RX chain with room for `max_payload` bytes of data.
    pub(super) fn next_rx(
        &mut self,
        max_payload: usize,
        now: Instant,
    ) -> Option<(Header, Vec<u8>)> {
        if let Some(hdr) = self.control.pop_front() {
            return Some((hdr, Vec::new()));
        }
        let start = self.cursor;
        let mut order: Vec<Key> = self.conns.keys().copied().collect();
        if let Some(cursor) = start {
            let split = order.partition_point(|k| *k <= cursor);
            order.rotate_left(split);
        }
        for key in order {
            let Some(mut conn) = self.conns.remove(&key) else {
                continue;
            };
            let packet = if conn.wants_rx() {
                conn.next_packet(max_payload)
            } else {
                None
            };
            let out = packet.map(|(op, flags, data)| {
                if op == OP_SHUTDOWN {
                    conn.state = State::Closing(now);
                }
                let hdr = self.conn_header(key, &mut conn, op, flags, data.len() as u32);
                (hdr, data)
            });
            self.conns.insert(key, conn);
            if out.is_some() {
                self.cursor = Some(key);
                return out;
            }
        }
        None
    }

    /// RST once the guest has shut down both directions and its data is written.
    fn finish_if_shut(&mut self, key: Key) {
        let done = self
            .conns
            .get(&key)
            .is_some_and(|c| c.peer_shutdown == SHUTDOWN_BOTH && c.pending_to_host() == 0);
        if done {
            self.close(key, "guest shut down both directions");
        }
    }

    /// Remove `key` and tell the guest with RST.
    fn close(&mut self, key: Key, reason: &'static str) {
        if self.conns.remove(&key).is_some() {
            self.closed(key, reason);
        }
        self.control.push_back(Header {
            src_cid: HOST_CID,
            dst_cid: self.guest_cid,
            src_port: key.0,
            dst_port: key.1,
            kind: TYPE_STREAM,
            op: OP_RST,
            ..Header::default()
        });
        self.stats.resets.fetch_add(1, Ordering::Relaxed);
    }

    fn closed(&self, key: Key, reason: &'static str) {
        tracing::info!(
            target: "ternvale::virtio::vsock",
            host_port = key.0,
            guest_port = key.1,
            reason,
            open = self.conns.len(),
            "vsock connection closed"
        );
    }

    /// RST in reply to a guest packet that has no usable connection.
    fn reply_rst(&mut self, to: &Header) {
        if to.op == OP_RST {
            return;
        }
        self.control.push_back(Header {
            src_cid: HOST_CID,
            dst_cid: self.guest_cid,
            src_port: to.dst_port,
            dst_port: to.src_port,
            kind: to.kind,
            op: OP_RST,
            ..Header::default()
        });
        self.stats.resets.fetch_add(1, Ordering::Relaxed);
    }

    fn conn_header(&self, key: Key, conn: &mut Conn, op: u16, flags: u32, len: u32) -> Header {
        let (buf_alloc, fwd_cnt) = conn.stamp();
        Header {
            src_cid: HOST_CID,
            dst_cid: self.guest_cid,
            src_port: key.0,
            dst_port: key.1,
            len,
            kind: TYPE_STREAM,
            op,
            flags,
            buf_alloc,
            fwd_cnt,
        }
    }

    fn alloc_key(&mut self, guest_port: u32) -> Key {
        loop {
            let port = self.next_port;
            self.next_port = self.next_port.checked_add(1).unwrap_or(EPHEMERAL_BASE);
            if !self.conns.contains_key(&(port, guest_port)) {
                return (port, guest_port);
            }
        }
    }
}

#[cfg(test)]
#[path = "muxer_tests.rs"]
mod tests;
