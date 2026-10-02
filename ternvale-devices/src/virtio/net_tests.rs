//! virtio-net through fake guest memory and the MMIO transport, with the loopback backend.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_vmm::{ExitEvent, GuestMemory, GuestRegs, MmioBus, MmioError, HOST_PAGE_SIZE};

use super::testutil::{arp_request, echo_request, GUEST_IP, GUEST_MAC};
use super::{
    open_backend, LoopbackBackend, NetError, PcapWriter, VirtioNet, GATEWAY_IP, GATEWAY_MAC,
    NET_HDR_LEN, VIRTIO_NET_F_MAC, VIRTIO_NET_F_MRG_RXBUF, VIRTIO_NET_F_STATUS, VIRTIO_NET_ID,
};
use crate::virtio::irq::VirtioIrq;
use crate::virtio::{QueueNotify, VirtioDevice, VirtioMmio};

const NEXT: u16 = 1;
const WRITE: u16 = 2;
const RX: (u64, u64, u64) = (0x000, 0x100, 0x200);
const TX: (u64, u64, u64) = (0x400, 0x500, 0x600);
const RX_BUF: u64 = 0x1000;
const TX_BUF: u64 = 0x3000;
const SLOT: u64 = 0x800;

struct Fx {
    memory: Arc<Mutex<GuestMemory>>,
    raised: Arc<AtomicBool>,
    base: u64,
}

impl Fx {
    fn new(pcap: Option<PcapWriter>) -> (Self, VirtioNet) {
        let mut mem = GuestMemory::new().expect("memory");
        let base = 0x4000_0000u64;
        mem.add_region(base, HOST_PAGE_SIZE * 2).expect("region");
        let memory = Arc::new(Mutex::new(mem));
        let irq = VirtioIrq::new();
        let raised = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&raised);
        irq.set_hook(Arc::new(move |level| flag.store(level, Ordering::Release)));
        let net = VirtioNet::open(
            GUEST_MAC,
            Box::new(LoopbackBackend::new()),
            Arc::clone(&memory),
            irq,
            pcap,
        )
        .expect("net");
        (
            Self {
                memory,
                raised,
                base,
            },
            net,
        )
    }

    fn desc(&self, table: u64, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let mut mem = self.memory.lock().expect("mem");
        let at = self.base + table + u64::from(index) * 16;
        mem.write_u64(at, addr).expect("addr");
        mem.write_u32(at + 8, len).expect("len");
        mem.write_u16(at + 12, flags).expect("flags");
        mem.write_u16(at + 14, next).expect("next");
    }

    fn publish(&self, avail: u64, slot: u16, head: u16) {
        let mut mem = self.memory.lock().expect("mem");
        let at = self.base + avail;
        mem.write_u16(at + 4 + u64::from(slot) * 2, head)
            .expect("ring");
        mem.write_u16(at + 2, slot + 1).expect("idx");
    }

    fn notify(&self, net: &mut VirtioNet, index: u16) {
        let (desc, avail, used) = if index == 0 { RX } else { TX };
        net.notify(QueueNotify {
            index,
            size: 4,
            desc: self.base + desc,
            avail: self.base + avail,
            used: self.base + used,
            features: 0,
        });
    }

    fn post_rx(&self, n: u16, len: u32) {
        self.desc(
            RX.0,
            n,
            self.base + RX_BUF + u64::from(n) * SLOT,
            len,
            WRITE,
            0,
        );
        self.publish(RX.1, n, n);
    }

    /// Header in descriptor `2n`, frame in `2n + 1` (any-layout split).
    fn send_tx(&self, n: u16, frame: &[u8], hdr: [u8; NET_HDR_LEN]) {
        let at = self.base + TX_BUF + u64::from(n) * SLOT;
        {
            let mut mem = self.memory.lock().expect("mem");
            mem.write_bytes(at, &hdr).expect("hdr");
            mem.write_bytes(at + 64, frame).expect("frame");
        }
        self.desc(TX.0, 2 * n, at, NET_HDR_LEN as u32, NEXT, 2 * n + 1);
        self.desc(TX.0, 2 * n + 1, at + 64, frame.len() as u32, 0, 0);
        self.publish(TX.1, n, 2 * n);
    }

    fn used(&self, ring: u64, slot: u16) -> (u32, u32) {
        let mem = self.memory.lock().expect("mem");
        let at = self.base + ring + 4 + u64::from(slot) * 8;
        (
            mem.read_u32(at).expect("id"),
            mem.read_u32(at + 4).expect("len"),
        )
    }

    fn wait_used(&self, ring: u64, want: u16) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let idx = {
                let mem = self.memory.lock().expect("mem");
                mem.read_u16(self.base + ring + 2).expect("idx")
            };
            if idx >= want {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("used ring at {ring:#x} did not reach {want}");
    }

    fn rx_bytes(&self, n: u16, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let mem = self.memory.lock().expect("mem");
        mem.read_bytes(self.base + RX_BUF + u64::from(n) * SLOT, &mut out)
            .expect("rx");
        out
    }
}

#[test]
fn mmio_reports_net_identity_features_and_mac() {
    let (fx, net) = Fx::new(None);
    let mut bus = MmioBus::new();
    VirtioMmio::register(&mut bus, 0, Box::new(net)).expect("register");
    let regs = Regs(RefCell::new([0; 31]));
    let read = |offset: u64, size: u8| {
        bus.dispatch(
            &regs,
            ExitEvent::Mmio {
                gpa: ternvale_vmm::VIRTIO_MMIO_BASE + offset,
                size,
                write: false,
                reg: 0,
            },
        )
        .expect("dispatch");
        regs.0.borrow()[0]
    };
    assert_eq!(read(0x008, 4), u64::from(VIRTIO_NET_ID));
    let features = read(0x010, 4);
    assert_ne!(features & VIRTIO_NET_F_MAC, 0, "MAC offered");
    assert_ne!(features & VIRTIO_NET_F_STATUS, 0, "STATUS offered");
    assert_eq!(features & VIRTIO_NET_F_MRG_RXBUF, 0, "MRG_RXBUF off");
    assert_eq!(
        read(0x100, 4),
        u64::from(u32::from_le_bytes([0x52, 0x54, 0x00, 0x12]))
    );
    assert_eq!(read(0x104, 2), u64::from(u16::from_le_bytes([0x34, 0x56])));
    assert_eq!(read(0x106, 2), 1, "link up");
    drop(fx);
}

#[test]
fn arp_request_round_trips_through_tx_and_rx_queues() {
    let (fx, mut net) = Fx::new(None);
    fx.post_rx(0, 0x800);
    fx.notify(&mut net, 0);
    let arp = arp_request(GATEWAY_IP);
    fx.send_tx(0, &arp, [0; NET_HDR_LEN]);
    fx.notify(&mut net, 1);
    fx.wait_used(TX.2, 1);
    fx.wait_used(RX.2, 1);
    assert_eq!(fx.used(TX.2, 0), (0, 0), "tx head 0 completed with len 0");
    let (id, len) = fx.used(RX.2, 0);
    assert_eq!((id, len), (0, (NET_HDR_LEN + 42) as u32));
    let bytes = fx.rx_bytes(0, len as usize);
    assert_eq!(&bytes[..10], &[0; 10], "no offload fields");
    assert_eq!(u16::from_le_bytes([bytes[10], bytes[11]]), 1, "num_buffers");
    let frame = &bytes[NET_HDR_LEN..];
    assert_eq!(&frame[0..6], &GUEST_MAC);
    assert_eq!(&frame[20..22], &[0, 2], "arp reply");
    assert_eq!(&frame[22..28], &GATEWAY_MAC);
    assert!(fx.raised.load(Ordering::Acquire), "used-buffer interrupt");
    let [tx_packets, tx_bytes, tx_dropped, rx_packets, rx_bytes, rx_dropped] =
        net.stats().snapshot();
    assert_eq!((tx_packets, tx_bytes, tx_dropped), (1, 42, 0));
    assert_eq!((rx_packets, rx_bytes, rx_dropped), (1, 42, 0));
}

#[test]
fn icmp_echo_round_trips_through_tx_and_rx_queues() {
    let (fx, mut net) = Fx::new(None);
    fx.post_rx(0, 0x800);
    fx.notify(&mut net, 0);
    let request = echo_request(7, b"ternvale ping");
    fx.send_tx(0, &request, [0; NET_HDR_LEN]);
    fx.notify(&mut net, 1);
    fx.wait_used(TX.2, 1);
    fx.wait_used(RX.2, 1);
    let (id, len) = fx.used(RX.2, 0);
    assert_eq!((id, len as usize), (0, NET_HDR_LEN + request.len()));
    let frame = fx.rx_bytes(0, len as usize).split_off(NET_HDR_LEN);
    assert_eq!(&frame[0..6], &GUEST_MAC, "eth dst");
    assert_eq!(&frame[6..12], &GATEWAY_MAC, "eth src");
    assert_eq!(&frame[12..14], &[0x08, 0x00], "ipv4");
    let ip = &frame[14..34];
    assert_eq!(ip[9], 1, "icmp");
    assert_eq!((&ip[12..16], &ip[16..20]), (&GATEWAY_IP[..], &GUEST_IP[..]));
    assert_eq!(super::packet::checksum(ip), 0, "ip header checksum");
    let icmp = &frame[34..];
    assert_eq!((icmp[0], icmp[1]), (0, 0), "echo reply");
    assert_eq!(&icmp[4..8], &request[38..42], "id and sequence echoed");
    assert_eq!(&icmp[8..], b"ternvale ping");
    assert_eq!(super::packet::checksum(icmp), 0, "icmp checksum");
    assert!(fx.raised.load(Ordering::Acquire), "used-buffer interrupt");
    let n = request.len() as u64;
    assert_eq!(net.stats().snapshot(), [1, n, 0, 1, n, 0]);
}

#[test]
fn echo_reply_waits_for_an_rx_buffer() {
    let (fx, mut net) = Fx::new(None);
    fx.send_tx(0, &echo_request(3, b"hold me"), [0; NET_HDR_LEN]);
    fx.notify(&mut net, 1);
    fx.wait_used(TX.2, 1);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(
        net.stats().snapshot()[3],
        0,
        "no rx before a buffer is posted"
    );
    fx.post_rx(0, 0x800);
    fx.notify(&mut net, 0);
    fx.wait_used(RX.2, 1);
    let (_, len) = fx.used(RX.2, 0);
    let frame = fx.rx_bytes(0, len as usize).split_off(NET_HDR_LEN);
    let ip = &frame[14..];
    assert_eq!(&ip[12..16], &GATEWAY_IP);
    assert_eq!(&ip[16..20], &GUEST_IP);
    assert_eq!(ip[20], 0, "icmp echo reply");
    assert_eq!(&ip[28..], b"hold me");
}

#[test]
fn offload_headers_and_small_rx_buffers_are_dropped() {
    let (fx, mut net) = Fx::new(None);
    fx.post_rx(0, 20);
    fx.notify(&mut net, 0);
    let mut gso = [0u8; NET_HDR_LEN];
    gso[1] = 1;
    fx.send_tx(0, &arp_request(GATEWAY_IP), gso);
    fx.send_tx(1, &arp_request(GATEWAY_IP), [0; NET_HDR_LEN]);
    fx.notify(&mut net, 1);
    fx.wait_used(TX.2, 2);
    fx.wait_used(RX.2, 1);
    assert_eq!(
        fx.used(RX.2, 0),
        (0, 0),
        "too-small rx buffer completed empty"
    );
    let [tx_packets, _, tx_dropped, rx_packets, _, rx_dropped] = net.stats().snapshot();
    assert_eq!((tx_packets, tx_dropped), (1, 1), "gso frame dropped");
    assert_eq!((rx_packets, rx_dropped), (0, 1));
}

#[test]
fn pcap_records_tx_and_rx_frames() {
    let path = std::env::temp_dir().join(format!("ternvale-net-{}.pcap", std::process::id()));
    let (fx, mut net) = Fx::new(Some(PcapWriter::create(&path).expect("pcap")));
    fx.post_rx(0, 0x800);
    fx.notify(&mut net, 0);
    fx.send_tx(0, &arp_request(GATEWAY_IP), [0; NET_HDR_LEN]);
    fx.notify(&mut net, 1);
    fx.wait_used(RX.2, 1);
    drop(net);
    let bytes = std::fs::read(&path).expect("read pcap");
    std::fs::remove_file(&path).expect("remove pcap");
    assert_eq!(bytes.len(), 24 + 2 * (16 + 42));
    let first = &bytes[24 + 16..24 + 16 + 42];
    let second = &bytes[24 + 2 * 16 + 42..];
    assert_eq!(&first[20..22], &[0, 1], "tx request first");
    assert_eq!(&second[20..22], &[0, 2], "rx reply second");
    drop(fx);
}

#[test]
fn backend_names() {
    assert_eq!(
        open_backend("loopback").expect("loopback").name(),
        "loopback"
    );
    assert!(matches!(
        open_backend("tap").err(),
        Some(NetError::UnknownBackend { .. })
    ));
    #[cfg(not(feature = "vmnet"))]
    assert!(matches!(
        open_backend("vmnet").err(),
        Some(NetError::Unsupported {
            backend: "vmnet",
            ..
        })
    ));
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
