//! Muxer logic against real Unix sockets in a temp directory.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{Muxer, EPHEMERAL_BASE, HANDSHAKE_TIMEOUT};
use crate::virtio::vsock::conn::BUF_ALLOC;
use crate::virtio::vsock::host::HostSide;
use crate::virtio::vsock::packet::{
    Header, HDR_LEN, OP_CREDIT_REQUEST, OP_CREDIT_UPDATE, OP_REQUEST, OP_RESPONSE, OP_RST, OP_RW,
    OP_SHUTDOWN, SHUTDOWN_BOTH, TYPE_STREAM,
};
use crate::virtio::vsock::{guest_socket_path, host_socket_path, VsockStats, HOST_CID};

const GUEST_CID: u32 = 3;
const RX_ROOM: usize = 4096 - HDR_LEN;

struct T {
    dir: PathBuf,
    mux: Muxer,
    stats: Arc<VsockStats>,
}

impl T {
    fn new(listen: &[u32]) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tvm-{}-{n}", std::process::id()));
        let host = HostSide::open(&dir, listen).expect("host side");
        let stats = Arc::new(VsockStats::default());
        let mux = Muxer::new(GUEST_CID, host, Arc::clone(&stats));
        Self { dir, mux, stats }
    }

    fn listen(&self, port: u32) -> UnixListener {
        UnixListener::bind(host_socket_path(&self.dir, port)).expect("host listener")
    }

    fn guest(&mut self, op: u16, src_port: u32, dst_port: u32, payload: &[u8], fwd_cnt: u32) {
        let hdr = Header {
            src_cid: u64::from(GUEST_CID),
            dst_cid: HOST_CID,
            src_port,
            dst_port,
            len: payload.len() as u32,
            kind: TYPE_STREAM,
            op,
            flags: if op == OP_SHUTDOWN { SHUTDOWN_BOTH } else { 0 },
            buf_alloc: 4096,
            fwd_cnt,
        };
        self.mux.handle_guest(&hdr, payload);
    }

    fn drain(&mut self) -> Vec<(Header, Vec<u8>)> {
        let mut out = Vec::new();
        while self.mux.has_rx() {
            match self.mux.next_rx(RX_ROOM, Instant::now()) {
                Some(p) => out.push(p),
                None => break,
            }
        }
        out
    }

    fn poll_drain(&mut self) -> Vec<(Header, Vec<u8>)> {
        self.mux.poll_host(Instant::now());
        self.drain()
    }
}

impl Drop for T {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.dir) {
            panic!("remove {}: {error}", self.dir.display());
        }
    }
}

fn ops(packets: &[(Header, Vec<u8>)]) -> Vec<u16> {
    packets.iter().map(|(h, _)| h.op).collect()
}

/// macOS rejects `SO_RCVTIMEO` (EINVAL) once the peer has closed, so set it up front.
fn timed(stream: UnixStream) -> UnixStream {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("timeout");
    stream
}

fn accept(listener: &UnixListener) -> UnixStream {
    timed(listener.accept().expect("accept").0)
}

fn connect(t: &T, port: u32) -> UnixStream {
    timed(UnixStream::connect(guest_socket_path(&t.dir, port)).expect("connect"))
}

fn read_exact(stream: &mut UnixStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    stream.read_exact(&mut buf).expect("host read");
    buf
}

fn assert_eof(stream: &mut UnixStream) {
    let mut buf = [0u8; 8];
    assert_eq!(
        stream.read(&mut buf).expect("read"),
        0,
        "host stream closed"
    );
}

#[test]
fn guest_connects_exchanges_data_and_shuts_down() {
    let mut t = T::new(&[]);
    let listener = t.listen(5000);
    t.guest(OP_REQUEST, 1025, 5000, b"", 0);
    let out = t.drain();
    assert_eq!(ops(&out), [OP_RESPONSE]);
    let h = out[0].0;
    assert_eq!((h.src_cid, h.dst_cid), (HOST_CID, u64::from(GUEST_CID)));
    assert_eq!((h.src_port, h.dst_port), (5000, 1025));
    assert_eq!((h.buf_alloc, h.fwd_cnt), (BUF_ALLOC, 0));
    let mut host = accept(&listener);

    t.guest(OP_RW, 1025, 5000, b"ping", 0);
    assert_eq!(read_exact(&mut host, 4), b"ping");
    host.write_all(b"pong").expect("host write");
    let out = t.poll_drain();
    assert_eq!(ops(&out), [OP_RW]);
    assert_eq!(out[0].1, b"pong");
    assert_eq!(
        (out[0].0.len, out[0].0.fwd_cnt),
        (4, 4),
        "fwd_cnt counts ping"
    );

    t.guest(OP_SHUTDOWN, 1025, 5000, b"", 4);
    assert_eq!(
        ops(&t.drain()),
        [OP_RST],
        "full shutdown is answered with RST"
    );
    assert_eof(&mut host);
    assert_eq!(t.mux.connections(), 0);
    let s = t.stats.snapshot();
    assert_eq!((s.connections, s.resets), (1, 1));
}

#[test]
fn guest_connect_without_a_host_listener_is_reset() {
    let mut t = T::new(&[]);
    t.guest(OP_REQUEST, 1025, 5001, b"", 0);
    let out = t.drain();
    assert_eq!(ops(&out), [OP_RST]);
    assert_eq!((out[0].0.src_port, out[0].0.dst_port), (5001, 1025));
    assert_eq!(t.mux.connections(), 0);
}

#[test]
fn host_connects_to_a_guest_port() {
    let mut t = T::new(&[1234]);
    let mut host = connect(&t, 1234);
    let out = t.poll_drain();
    assert_eq!(ops(&out), [OP_REQUEST]);
    let req = out[0].0;
    assert_eq!(req.dst_port, 1234);
    assert!(req.src_port >= EPHEMERAL_BASE, "ephemeral host port");
    let eph = req.src_port;

    t.guest(OP_RESPONSE, 1234, eph, b"", 0);
    host.write_all(b"hello").expect("host write");
    let out = t.poll_drain();
    assert_eq!(ops(&out), [OP_RW]);
    assert_eq!(out[0].1, b"hello");
    t.guest(OP_RW, 1234, eph, b"hi", 5);
    assert_eq!(read_exact(&mut host, 2), b"hi");

    drop(host);
    let out = t.poll_drain();
    assert_eq!(ops(&out), [OP_SHUTDOWN], "host EOF becomes SHUTDOWN");
    assert_eq!(out[0].0.flags, SHUTDOWN_BOTH);
    t.guest(OP_RST, 1234, eph, b"", 5);
    assert_eq!(t.mux.connections(), 0);
    assert!(t.drain().is_empty(), "RST is never answered");
}

#[test]
fn unanswered_host_request_times_out_with_rst() {
    let mut t = T::new(&[1234]);
    let mut host = connect(&t, 1234);
    let now = Instant::now();
    t.mux.poll_host(now);
    assert_eq!(ops(&t.drain()), [OP_REQUEST]);
    t.mux
        .poll_host(now + HANDSHAKE_TIMEOUT + Duration::from_millis(1));
    assert_eq!(ops(&t.drain()), [OP_RST]);
    assert_eof(&mut host);
}

#[test]
fn host_data_respects_guest_credit_and_credit_requests() {
    let mut t = T::new(&[]);
    let listener = t.listen(5002);
    let hdr_alloc = |t: &mut T, op, fwd| {
        let hdr = Header {
            src_cid: u64::from(GUEST_CID),
            dst_cid: HOST_CID,
            src_port: 1025,
            dst_port: 5002,
            kind: TYPE_STREAM,
            op,
            buf_alloc: 8,
            fwd_cnt: fwd,
            ..Header::default()
        };
        t.mux.handle_guest(&hdr, b"");
    };
    hdr_alloc(&mut t, OP_REQUEST, 0);
    assert_eq!(ops(&t.drain()), [OP_RESPONSE]);
    let mut host = accept(&listener);
    host.write_all(&[7u8; 20]).expect("host write");
    let out = t.poll_drain();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].1.len(), 8, "only the guest's 8-byte window");
    assert!(t.poll_drain().is_empty(), "no credit, nothing sent");
    hdr_alloc(&mut t, OP_CREDIT_UPDATE, 8);
    let out = t.poll_drain();
    assert_eq!(out[0].1.len(), 8, "credit update reopened the window");
    hdr_alloc(&mut t, OP_CREDIT_UPDATE, 16);
    let out = t.poll_drain();
    assert_eq!((out[0].0.op, out[0].1.len()), (OP_RW, 4), "last 4 bytes");
    hdr_alloc(&mut t, OP_CREDIT_REQUEST, 20);
    let out = t.poll_drain();
    assert_eq!(ops(&out), [OP_CREDIT_UPDATE], "credit request answered");
    assert_eq!((out[0].0.buf_alloc, out[0].0.fwd_cnt), (BUF_ALLOC, 0));
}

#[test]
fn forwarding_a_quarter_window_sends_a_credit_update() {
    let mut t = T::new(&[]);
    let listener = t.listen(5003);
    t.guest(OP_REQUEST, 1025, 5003, b"", 0);
    t.drain();
    let mut host = accept(&listener);
    host.set_nonblocking(true).expect("nonblocking");
    let chunk = vec![1u8; 40_000];
    t.guest(OP_RW, 1025, 5003, &chunk, 0);
    t.guest(OP_RW, 1025, 5003, &chunk, 0);
    let mut got = 0;
    let mut buf = vec![0u8; 65536];
    let deadline = Instant::now() + Duration::from_secs(5);
    while got < 80_000 && Instant::now() < deadline {
        match host.read(&mut buf) {
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("host read: {e}"),
        }
        t.mux.poll_host(Instant::now());
    }
    assert_eq!(got, 80_000);
    let out = t.drain();
    assert_eq!(ops(&out), [OP_CREDIT_UPDATE]);
    assert_eq!(out[0].0.fwd_cnt, 80_000);
}

#[test]
fn guest_exceeding_its_credit_is_reset() {
    let mut t = T::new(&[]);
    let _listener = t.listen(5004);
    t.guest(OP_REQUEST, 1025, 5004, b"", 0);
    t.drain();
    let chunk = vec![2u8; 64 * 1024];
    let mut reset = false;
    for _ in 0..16 {
        t.guest(OP_RW, 1025, 5004, &chunk, 0);
        if ops(&t.drain()).contains(&OP_RST) {
            reset = true;
            break;
        }
    }
    assert!(reset, "host never reads, so the window must overflow");
    assert_eq!(t.mux.connections(), 0);
}

#[test]
fn misaddressed_unknown_and_seqpacket_packets() {
    let mut t = T::new(&[]);
    let wrong = Header {
        src_cid: 9,
        dst_cid: HOST_CID,
        kind: TYPE_STREAM,
        op: OP_REQUEST,
        ..Header::default()
    };
    t.mux.handle_guest(&wrong, b"");
    assert!(t.drain().is_empty(), "wrong source cid dropped");
    assert_eq!(t.stats.snapshot().dropped, 1);

    t.guest(OP_RW, 1025, 6000, b"x", 0);
    assert_eq!(ops(&t.drain()), [OP_RST], "data for no connection");

    let seq = Header {
        src_cid: u64::from(GUEST_CID),
        dst_cid: HOST_CID,
        kind: 2,
        op: OP_REQUEST,
        ..Header::default()
    };
    t.mux.handle_guest(&seq, b"");
    let out = t.drain();
    assert_eq!(
        (out[0].0.op, out[0].0.kind),
        (OP_RST, 2),
        "seqpacket refused"
    );
}

#[test]
fn guest_rst_closes_the_host_stream() {
    let mut t = T::new(&[]);
    let listener = t.listen(5005);
    t.guest(OP_REQUEST, 1025, 5005, b"", 0);
    t.drain();
    let mut host = accept(&listener);
    t.guest(OP_RST, 1025, 5005, b"", 0);
    assert!(t.drain().is_empty());
    assert_eof(&mut host);
}
