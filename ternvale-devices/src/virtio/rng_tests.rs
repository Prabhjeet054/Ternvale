//! virtio-rng through the MMIO bus and a split queue in fake guest memory.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ternvale_vmm::{ExitEvent, GuestMemory, GuestRegs, MmioBus, MmioError, HOST_PAGE_SIZE};

use super::{VirtioRng, RNG_MAX_PER_CHAIN, VIRTIO_RNG_ID};
use crate::virtio::irq::VirtioIrq;
use crate::virtio::{QueueNotify, VirtioDevice, VirtioMmio};

const BASE: u64 = 0x4000_0000;
const DESC: u64 = BASE;
const AVAIL: u64 = BASE + 0x100;
const USED: u64 = BASE + 0x200;
const BUF: u64 = BASE + 0x1000;
const NEXT: u16 = 1;
const WRITE: u16 = 2;

struct Fx {
    memory: Arc<Mutex<GuestMemory>>,
    raised: Arc<AtomicBool>,
    rng: VirtioRng,
}

impl Fx {
    fn new(pages: u64) -> Self {
        let mut mem = GuestMemory::new().expect("memory");
        mem.add_region(BASE, HOST_PAGE_SIZE * pages)
            .expect("region");
        let memory = Arc::new(Mutex::new(mem));
        let irq = VirtioIrq::new();
        let raised = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&raised);
        irq.set_hook(Arc::new(move |level| flag.store(level, Ordering::Release)));
        let rng = VirtioRng::new(Arc::clone(&memory), irq);
        Self {
            memory,
            raised,
            rng,
        }
    }

    fn desc(&self, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let mut mem = self.memory.lock().expect("mem");
        let at = DESC + u64::from(index) * 16;
        mem.write_u64(at, addr).expect("addr");
        mem.write_u32(at + 8, len).expect("len");
        mem.write_u16(at + 12, flags).expect("flags");
        mem.write_u16(at + 14, next).expect("next");
    }

    fn publish(&self, heads: &[u16]) {
        let mut mem = self.memory.lock().expect("mem");
        for (slot, head) in heads.iter().enumerate() {
            mem.write_u16(AVAIL + 4 + slot as u64 * 2, *head)
                .expect("ring");
        }
        mem.write_u16(AVAIL + 2, heads.len() as u16).expect("idx");
    }

    fn kick(&mut self) {
        self.rng.notify(QueueNotify {
            index: 0,
            size: 8,
            desc: DESC,
            avail: AVAIL,
            used: USED,
            features: 0,
        });
    }

    fn used(&self, slot: u16) -> (u32, u32) {
        let mem = self.memory.lock().expect("mem");
        let at = USED + 4 + u64::from(slot) * 8;
        (
            mem.read_u32(at).expect("id"),
            mem.read_u32(at + 4).expect("len"),
        )
    }

    fn bytes(&self, addr: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let mem = self.memory.lock().expect("mem");
        mem.read_bytes(addr, &mut out).expect("read");
        out
    }
}

#[test]
fn mmio_reports_entropy_device_with_one_queue() {
    let mut fx = Fx::new(1);
    let rng = std::mem::replace(
        &mut fx.rng,
        VirtioRng::new(Arc::clone(&fx.memory), VirtioIrq::new()),
    );
    let mut bus = MmioBus::new();
    VirtioMmio::register(&mut bus, 3, Box::new(rng)).expect("register");
    let regs = Regs(RefCell::new([0; 31]));
    let base = ternvale_vmm::VIRTIO_MMIO_BASE + 3 * ternvale_vmm::VIRTIO_MMIO_SLOT_SIZE;
    let read = |offset: u64| {
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
    assert_eq!(read(0x000), 0x7472_6976, "magic");
    assert_eq!(read(0x008), u64::from(VIRTIO_RNG_ID));
    assert_eq!(read(0x010), 0, "no device features in bank 0");
    assert_eq!(read(0x034), 256, "queue 0 max size");
}

#[test]
fn fills_single_and_chained_writable_buffers() {
    let mut fx = Fx::new(1);
    fx.desc(0, BUF, 64, WRITE, 0);
    fx.desc(1, BUF + 0x100, 16, WRITE | NEXT, 2);
    fx.desc(2, BUF + 0x200, 48, WRITE, 0);
    fx.publish(&[0, 1]);
    fx.kick();
    assert_eq!(fx.used(0), (0, 64));
    assert_eq!(fx.used(1), (1, 64));
    let a = fx.bytes(BUF, 64);
    let b = [fx.bytes(BUF + 0x100, 16), fx.bytes(BUF + 0x200, 48)].concat();
    assert!(a.iter().any(|&x| x != 0) && b.iter().any(|&x| x != 0));
    assert_ne!(a, b, "two requests got different bytes");
    assert!(fx.raised.load(Ordering::Acquire), "used-buffer interrupt");
    assert_eq!(fx.rng.stats().snapshot(), [2, 128, 0]);
}

// Addresses outside guest memory never reach the device: `SplitQueue` rejects
// them and marks the queue broken (see queue_tests.rs).
#[test]
fn readable_buffers_complete_empty() {
    let mut fx = Fx::new(1);
    fx.desc(0, BUF, 32, 0, 0);
    fx.desc(1, BUF + 0x100, 32, WRITE, 0);
    fx.publish(&[0, 1]);
    fx.kick();
    assert_eq!(fx.used(0), (0, 0), "device-readable buffer");
    assert_eq!(fx.used(1), (1, 32), "next request still served");
    assert!(
        fx.bytes(BUF, 32).iter().all(|&x| x == 0),
        "readable buffer untouched"
    );
    assert_eq!(fx.rng.stats().snapshot(), [1, 32, 1]);
}

#[test]
fn large_requests_are_capped() {
    let mut fx = Fx::new(6);
    let len = (RNG_MAX_PER_CHAIN + 4096) as u32;
    fx.desc(0, BUF, len, WRITE, 0);
    fx.publish(&[0]);
    fx.kick();
    assert_eq!(fx.used(0), (0, RNG_MAX_PER_CHAIN as u32));
    let tail = fx.bytes(BUF + RNG_MAX_PER_CHAIN as u64, 4096);
    assert!(tail.iter().all(|&x| x == 0), "bytes past the cap untouched");
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
