//! virtio-vsock through the MMIO bus and split rings in fake guest memory.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_vmm::{ExitEvent, GuestMemory, GuestRegs, MmioBus, MmioError, HOST_PAGE_SIZE};

use super::packet::{Header, HDR_LEN, OP_REQUEST, OP_RESPONSE, OP_RW, TYPE_STREAM};
use super::{
    guest_socket_path, host_socket_path, VirtioVsock, VsockConfig, HOST_CID, VIRTIO_VSOCK_F_STREAM,
    VIRTIO_VSOCK_ID,
};
use crate::virtio::irq::VirtioIrq;
use crate::virtio::{QueueNotify, VirtioDevice, VirtioMmio};

const BASE: u64 = 0x4000_0000;
const NEXT: u16 = 1;
const WRITE: u16 = 2;
const RX: (u64, u64, u64) = (0x000, 0x100, 0x200);
const TX: (u64, u64, u64) = (0x400, 0x500, 0x600);
const RX_BUF: u64 = 0x1000;
const RX_LEN: u32 = 0x1000;
const TX_BUF: u64 = 0x8000;

fn uds_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("tvd-{}-{n}", std::process::id()))
}

struct Fx {
    memory: Arc<Mutex<GuestMemory>>,
    raised: Arc<AtomicBool>,
    dir: PathBuf,
    cid: u32,
    tx_next: u16,
}

impl Fx {
    fn new(cid: u32, listen: &[u32]) -> (Self, VirtioVsock) {
        let mut mem = GuestMemory::new().expect("memory");
        mem.add_region(BASE, HOST_PAGE_SIZE * 3).expect("region");
        let memory = Arc::new(Mutex::new(mem));
        let irq = VirtioIrq::new();
        let raised = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&raised);
        irq.set_hook(Arc::new(move |level| flag.store(level, Ordering::Release)));
        let dir = uds_dir();
        let config = VsockConfig {
            guest_cid: Some(cid),
            uds_dir: dir.clone(),
            listen_ports: listen.to_vec(),
        };
        let dev = VirtioVsock::open(&config, Arc::clone(&memory), irq).expect("vsock");
        let fx = Self {
            memory,
            raised,
            dir,
            cid,
            tx_next: 0,
        };
        (fx, dev)
    }

    fn desc(&self, table: u64, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let mut mem = self.memory.lock().expect("mem");
        let at = BASE + table + u64::from(index) * 16;
        mem.write_u64(at, addr).expect("addr");
        mem.write_u32(at + 8, len).expect("len");
        mem.write_u16(at + 12, flags).expect("flags");
        mem.write_u16(at + 14, next).expect("next");
    }

    fn publish(&self, avail: u64, slot: u16, head: u16) {
        let mut mem = self.memory.lock().expect("mem");
        mem.write_u16(BASE + avail + 4 + u64::from(slot) * 2, head)
            .expect("ring");
        mem.write_u16(BASE + avail + 2, slot + 1).expect("idx");
    }

    fn notify(&self, dev: &mut VirtioVsock, index: u16) {
        let (desc, avail, used) = if index == 0 { RX } else { TX };
        dev.notify(QueueNotify {
            index,
            size: 8,
            desc: BASE + desc,
            avail: BASE + avail,
            used: BASE + used,
            features: 0,
        });
    }

    fn post_rx(&self, dev: &mut VirtioVsock, count: u16) {
        for n in 0..count {
            let addr = BASE + RX_BUF + u64::from(n) * u64::from(RX_LEN);
            self.desc(RX.0, n, addr, RX_LEN, WRITE, 0);
            self.publish(RX.1, n, n);
        }
        self.notify(dev, 0);
    }

    /// Header in descriptor `2n`, payload (if any) in `2n + 1`.
    fn send(&mut self, dev: &mut VirtioVsock, op: u16, ports: (u32, u32), payload: &[u8]) {
        let n = self.tx_next;
        self.tx_next += 1;
        let hdr = Header {
            src_cid: u64::from(self.cid),
            dst_cid: HOST_CID,
            src_port: ports.0,
            dst_port: ports.1,
            len: payload.len() as u32,
            kind: TYPE_STREAM,
            op,
            buf_alloc: 65536,
            ..Header::default()
        };
        let at = BASE + TX_BUF + u64::from(n) * 0x200;
        {
            let mut mem = self.memory.lock().expect("mem");
            mem.write_bytes(at, &hdr.encode()).expect("hdr");
            mem.write_bytes(at + 0x80, payload).expect("payload");
        }
        let flags = if payload.is_empty() { 0 } else { NEXT };
        self.desc(TX.0, 2 * n, at, HDR_LEN as u32, flags, 2 * n + 1);
        self.desc(TX.0, 2 * n + 1, at + 0x80, payload.len() as u32, 0, 0);
        self.publish(TX.1, n, 2 * n);
        self.notify(dev, 1);
    }

    /// Wait for RX used slot `slot`, then decode that buffer.
    fn rx(&self, slot: u16) -> (Header, Vec<u8>) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let idx = {
                let mem = self.memory.lock().expect("mem");
                mem.read_u16(BASE + RX.2 + 2).expect("idx")
            };
            if idx > slot {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "rx used slot {slot} never filled"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let mem = self.memory.lock().expect("mem");
        let at = BASE + RX.2 + 4 + u64::from(slot) * 8;
        let id = mem.read_u32(at).expect("id");
        let len = mem.read_u32(at + 4).expect("len") as usize;
        let mut bytes = vec![0u8; len];
        mem.read_bytes(
            BASE + RX_BUF + u64::from(id) * u64::from(RX_LEN),
            &mut bytes,
        )
        .expect("rx");
        let hdr = Header::parse(&bytes).expect("rx header");
        (hdr, bytes.split_off(HDR_LEN))
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.dir) {
            panic!("remove {}: {error}", self.dir.display());
        }
    }
}

fn read_n(stream: &mut UnixStream, n: usize) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("timeout");
    let mut buf = vec![0u8; n];
    stream.read_exact(&mut buf).expect("host read");
    buf
}

#[test]
fn mmio_reports_vsock_identity_and_guest_cid() {
    let (fx, dev) = Fx::new(70_010, &[]);
    let mut bus = MmioBus::new();
    VirtioMmio::register(&mut bus, 1, Box::new(dev)).expect("register");
    let regs = Regs(RefCell::new([0; 31]));
    let base = ternvale_vmm::VIRTIO_MMIO_BASE + ternvale_vmm::VIRTIO_MMIO_SLOT_SIZE;
    let mut read = |offset: u64| {
        bus.dispatch(
            &regs,
            ExitEvent::Mmio {
                gpa: base + offset,
                size: 4,
                write: false,
                reg: 0,
            },
        )
        .expect("dispatch");
        regs.0.borrow()[0]
    };
    assert_eq!(read(0x008), u64::from(VIRTIO_VSOCK_ID));
    assert_ne!(read(0x010) & VIRTIO_VSOCK_F_STREAM, 0, "stream offered");
    assert_eq!(read(0x100), 70_010, "guest_cid low word");
    assert_eq!(read(0x104), 0, "guest_cid high word");
    drop(bus);
    drop(fx);
}

#[test]
fn guest_stream_round_trips_through_the_queues() {
    let (mut fx, mut dev) = Fx::new(70_011, &[]);
    let listener = UnixListener::bind(host_socket_path(&fx.dir, 5000)).expect("listen");
    fx.post_rx(&mut dev, 4);
    fx.send(&mut dev, OP_REQUEST, (1025, 5000), b"");
    let (resp, _) = fx.rx(0);
    assert_eq!(resp.op, OP_RESPONSE);
    assert_eq!((resp.src_cid, resp.dst_cid), (HOST_CID, 70_011));
    let (mut host, _) = listener.accept().expect("accept");

    fx.send(&mut dev, OP_RW, (1025, 5000), b"ping");
    assert_eq!(read_n(&mut host, 4), b"ping");
    host.write_all(b"pong").expect("host write");
    let (data, payload) = fx.rx(1);
    assert_eq!((data.op, data.len, data.dst_port), (OP_RW, 4, 1025));
    assert_eq!(payload, b"pong");
    assert!(fx.raised.load(Ordering::Acquire), "used-buffer interrupt");
    let s = dev.stats().snapshot();
    assert_eq!(
        (s.tx_packets, s.tx_bytes, s.rx_packets, s.rx_bytes),
        (2, 4, 2, 4)
    );
    assert_eq!((s.connections, s.dropped), (1, 0));
}

#[test]
fn host_initiated_stream_through_the_queues() {
    let (mut fx, mut dev) = Fx::new(70_012, &[1234]);
    fx.post_rx(&mut dev, 4);
    let mut host = UnixStream::connect(guest_socket_path(&fx.dir, 1234)).expect("connect");
    let (req, _) = fx.rx(0);
    assert_eq!(
        (req.op, req.dst_port, req.dst_cid),
        (OP_REQUEST, 1234, 70_012)
    );
    fx.send(&mut dev, OP_RESPONSE, (1234, req.src_port), b"");
    host.write_all(b"hi").expect("host write");
    let (data, payload) = fx.rx(1);
    assert_eq!((data.op, payload.as_slice()), (OP_RW, &b"hi"[..]));
    fx.send(&mut dev, OP_RW, (1234, req.src_port), b"yo");
    assert_eq!(read_n(&mut host, 2), b"yo");
}

struct Regs(RefCell<[u64; 31]>);

impl GuestRegs for Regs {
    fn get_reg(&self, index: u8) -> Result<u64, MmioError> {
        Ok(self.0.borrow()[usize::from(index)])
    }

    fn set_reg(&self, index: u8, value: u64) -> Result<(), MmioError> {
        self.0.borrow_mut()[usize::from(index)] = value;
        Ok(())
    }
}
