//! One vsock stream: the host Unix stream plus credit accounting (virtio 1.2, 5.10.6.3).
//!
//! The guest may have at most `buf_alloc - (rx_cnt - fwd_cnt)` bytes in flight
//! toward the host, where `fwd_cnt` counts bytes written to the Unix stream.
//! Toward the guest, Ternvale sends at most `peer_buf_alloc - (tx_cnt -
//! peer_fwd_cnt)`. All counters wrap modulo 2^32.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::time::Instant;

use super::packet::{
    OP_CREDIT_UPDATE, OP_RW, OP_SHUTDOWN, SHUTDOWN_BOTH, SHUTDOWN_RCV, SHUTDOWN_SEND,
};

/// Receive buffer Ternvale advertises per connection.
pub(super) const BUF_ALLOC: u32 = 256 * 1024;
/// Most host bytes staged per connection before the guest takes them.
pub(super) const STAGE_MAX: usize = 64 * 1024;
/// Send an unsolicited credit update after this many bytes are forwarded.
const CREDIT_UPDATE_STEP: u32 = BUF_ALLOC / 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum State {
    /// Host-initiated; REQUEST sent, waiting for RESPONSE.
    Connecting(Instant),
    Established,
    /// Ternvale sent SHUTDOWN after host EOF; waiting for the guest's RST.
    Closing(Instant),
}

pub(super) struct Conn {
    stream: UnixStream,
    pub(super) state: State,
    peer_buf_alloc: u32,
    peer_fwd_cnt: u32,
    tx_cnt: u32,
    rx_cnt: u32,
    fwd_cnt: u32,
    last_fwd_sent: u32,
    to_host: VecDeque<u8>,
    to_guest: VecDeque<u8>,
    pub(super) peer_shutdown: u32,
    host_write_shut: bool,
    host_eof: bool,
    shutdown_sent: bool,
    credit_requested: bool,
}

impl Conn {
    pub(super) fn new(stream: UnixStream, state: State) -> Self {
        Self {
            stream,
            state,
            peer_buf_alloc: 0,
            peer_fwd_cnt: 0,
            tx_cnt: 0,
            rx_cnt: 0,
            fwd_cnt: 0,
            last_fwd_sent: 0,
            to_host: VecDeque::new(),
            to_guest: VecDeque::new(),
            peer_shutdown: 0,
            host_write_shut: false,
            host_eof: false,
            shutdown_sent: false,
            credit_requested: false,
        }
    }

    /// Every guest header carries its current receive window.
    pub(super) fn update_peer(&mut self, buf_alloc: u32, fwd_cnt: u32) {
        self.peer_buf_alloc = buf_alloc;
        self.peer_fwd_cnt = fwd_cnt;
    }

    /// Bytes the guest can still accept.
    pub(super) fn peer_credit(&self) -> u32 {
        let in_flight = self.tx_cnt.wrapping_sub(self.peer_fwd_cnt);
        self.peer_buf_alloc.saturating_sub(in_flight)
    }

    /// Queue guest data for the host. `false` when it exceeds the advertised window.
    pub(super) fn accept_payload(&mut self, data: &[u8]) -> bool {
        if self.to_host.len() + data.len() > BUF_ALLOC as usize {
            return false;
        }
        self.to_host.extend(data);
        self.rx_cnt = self.rx_cnt.wrapping_add(data.len() as u32);
        true
    }

    pub(super) fn pending_to_host(&self) -> usize {
        self.to_host.len()
    }

    /// Write queued guest data to the Unix stream without blocking. After the
    /// guest's SHUTDOWN_SEND and a drained queue, half-close the host side.
    pub(super) fn flush(&mut self) -> std::io::Result<usize> {
        let mut written = 0;
        while !self.to_host.is_empty() {
            let (front, _) = self.to_host.as_slices();
            match self.stream.write(front) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(n) => {
                    self.to_host.drain(..n);
                    self.fwd_cnt = self.fwd_cnt.wrapping_add(n as u32);
                    written += n;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        if self.peer_shutdown & SHUTDOWN_SEND != 0
            && self.to_host.is_empty()
            && !self.host_write_shut
        {
            self.host_write_shut = true;
            match self.stream.shutdown(Shutdown::Write) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotConnected => {}
                Err(e) => return Err(e),
            }
        }
        Ok(written)
    }

    /// Read host data into the staging buffer, bounded by the guest's credit.
    pub(super) fn fill(&mut self) -> std::io::Result<usize> {
        if self.state != State::Established
            || self.host_eof
            || self.peer_shutdown & SHUTDOWN_RCV != 0
        {
            return Ok(0);
        }
        let credit = (self.peer_credit() as usize).saturating_sub(self.to_guest.len());
        let room = credit.min(STAGE_MAX.saturating_sub(self.to_guest.len()));
        let mut buf = vec![0u8; room];
        let mut total = 0;
        while total < room {
            match self.stream.read(&mut buf[total..]) {
                Ok(0) => {
                    self.host_eof = true;
                    break;
                }
                Ok(n) => total += n,
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        self.to_guest.extend(&buf[..total]);
        Ok(total)
    }

    pub(super) fn request_credit_update(&mut self) {
        self.credit_requested = true;
    }

    fn credit_update_due(&self) -> bool {
        self.credit_requested || self.fwd_cnt.wrapping_sub(self.last_fwd_sent) >= CREDIT_UPDATE_STEP
    }

    fn shutdown_due(&self) -> bool {
        self.host_eof && self.to_guest.is_empty() && !self.shutdown_sent
    }

    /// Whether [`Conn::next_packet`] has something for the guest.
    pub(super) fn wants_rx(&self) -> bool {
        (!self.to_guest.is_empty() && self.peer_credit() > 0)
            || self.credit_update_due()
            || self.shutdown_due()
    }

    /// Next `(op, flags, payload)` for the guest: data first, then a credit
    /// update, then SHUTDOWN after host EOF once staged data is gone.
    pub(super) fn next_packet(&mut self, max_payload: usize) -> Option<(u16, u32, Vec<u8>)> {
        let n = self
            .to_guest
            .len()
            .min(max_payload)
            .min(self.peer_credit() as usize);
        if n > 0 {
            let data: Vec<u8> = self.to_guest.drain(..n).collect();
            self.tx_cnt = self.tx_cnt.wrapping_add(n as u32);
            return Some((OP_RW, 0, data));
        }
        if self.credit_update_due() {
            return Some((OP_CREDIT_UPDATE, 0, Vec::new()));
        }
        if self.shutdown_due() {
            self.shutdown_sent = true;
            return Some((OP_SHUTDOWN, SHUTDOWN_BOTH, Vec::new()));
        }
        None
    }

    /// `(buf_alloc, fwd_cnt)` for an outgoing header. Every header is a credit update.
    pub(super) fn stamp(&mut self) -> (u32, u32) {
        self.last_fwd_sent = self.fwd_cnt;
        self.credit_requested = false;
        (BUF_ALLOC, self.fwd_cnt)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    use super::{Conn, State, BUF_ALLOC};
    use crate::virtio::vsock::packet::{OP_CREDIT_UPDATE, OP_RW, OP_SHUTDOWN};

    fn pair() -> (Conn, UnixStream) {
        let (a, b) = UnixStream::pair().expect("pair");
        a.set_nonblocking(true).expect("nonblocking");
        (Conn::new(a, State::Established), b)
    }

    #[test]
    fn peer_credit_wraps_modulo_two_to_the_32() {
        let (mut c, _peer) = pair();
        c.tx_cnt = 5;
        c.update_peer(100, u32::MAX - 9);
        assert_eq!(
            c.peer_credit(),
            100 - 15,
            "15 bytes in flight across the wrap"
        );
        c.update_peer(10, 0);
        assert_eq!(c.peer_credit(), 5);
        c.update_peer(2, 0);
        assert_eq!(c.peer_credit(), 0, "window shrank below in-flight bytes");
    }

    #[test]
    fn host_data_is_limited_by_guest_credit() {
        let (mut c, mut peer) = pair();
        c.update_peer(4, 0);
        peer.write_all(b"abcdefgh").expect("write");
        assert_eq!(c.fill().expect("fill"), 4);
        assert_eq!(c.next_packet(1024), Some((OP_RW, 0, b"abcd".to_vec())));
        assert!(!c.wants_rx(), "no credit left");
        c.update_peer(4, 4);
        assert_eq!(c.fill().expect("fill"), 4);
        assert_eq!(
            c.next_packet(2).map(|p| p.2),
            Some(b"ef".to_vec()),
            "chain cap"
        );
        assert_eq!(c.next_packet(8).map(|p| p.2), Some(b"gh".to_vec()));
    }

    #[test]
    fn guest_window_and_credit_updates() {
        let (mut c, mut peer) = pair();
        assert!(c.accept_payload(&vec![1u8; BUF_ALLOC as usize]));
        assert!(!c.accept_payload(&[1]), "one byte past the window");
        let mut sink = vec![0u8; BUF_ALLOC as usize];
        let mut got = 0;
        while c.pending_to_host() > 0 {
            c.flush().expect("flush");
            got += peer.read(&mut sink[got..]).expect("read");
        }
        assert_eq!(c.fwd_cnt, BUF_ALLOC);
        assert!(c.wants_rx(), "forwarded a quarter window or more");
        assert_eq!(c.next_packet(0).map(|p| p.0), Some(OP_CREDIT_UPDATE));
        assert_eq!(c.stamp(), (BUF_ALLOC, BUF_ALLOC));
        assert!(!c.wants_rx());
        c.request_credit_update();
        assert_eq!(c.next_packet(0).map(|p| p.0), Some(OP_CREDIT_UPDATE));
    }

    #[test]
    fn host_eof_sends_shutdown_after_staged_data() {
        let (mut c, mut peer) = pair();
        c.update_peer(1024, 0);
        peer.write_all(b"bye").expect("write");
        drop(peer);
        c.fill().expect("fill");
        assert_eq!(c.next_packet(64).map(|p| p.0), Some(OP_RW));
        assert_eq!(
            c.next_packet(64).map(|p| (p.0, p.1)),
            Some((OP_SHUTDOWN, 3))
        );
        assert_eq!(c.next_packet(64), None, "shutdown sent once");
    }
}
