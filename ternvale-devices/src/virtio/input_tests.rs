//! virtio-input through the MMIO bus and split queues in fake guest memory.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ternvale_vmm::{ExitEvent, GuestMemory, GuestRegs, MmioBus, MmioError, HOST_PAGE_SIZE};

use super::*;
use crate::virtio::irq::VirtioIrq;
use crate::virtio::{QueueNotify, VirtioDevice, VirtioMmio};

const BASE: u64 = 0x4000_0000;
const BUF: u64 = BASE + 0x4000;
const WRITE: u16 = 2;

/// `(desc, avail, used)` for queue `index`.
fn ring(index: u16) -> (u64, u64, u64) {
    let at = BASE + u64::from(index) * 0x1000;
    (at, at + 0x400, at + 0x800)
}

struct Fx {
    memory: Arc<Mutex<GuestMemory>>,
    raised: Arc<AtomicBool>,
    dev: VirtioInput,
    sink: InputSink,
    published: [u16; 2],
}

impl Fx {
    fn new(kind: InputKind) -> Self {
        let mut mem = GuestMemory::new().expect("memory");
        mem.add_region(BASE, HOST_PAGE_SIZE * 2).expect("region");
        let memory = Arc::new(Mutex::new(mem));
        let irq = VirtioIrq::new();
        let raised = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&raised);
        irq.set_hook(Arc::new(move |level| flag.store(level, Ordering::Release)));
        let dev = VirtioInput::new(kind, Arc::clone(&memory), irq);
        let sink = dev.sink();
        Self {
            memory,
            raised,
            dev,
            sink,
            published: [0; 2],
        }
    }

    /// Post one 8-byte buffer per descriptor `first..first+count` on `queue`.
    fn post(&mut self, queue: u16, first: u16, count: u16, flags: u16) {
        let (desc, avail, _) = ring(queue);
        let mut mem = self.memory.lock().expect("mem");
        for i in first..first + count {
            let at = desc + u64::from(i) * 16;
            mem.write_u64(at, buf(queue, i)).expect("addr");
            mem.write_u32(at + 8, 8).expect("len");
            mem.write_u16(at + 12, flags).expect("flags");
            mem.write_u16(at + 14, 0).expect("next");
            let slot = self.published[usize::from(queue)];
            mem.write_u16(avail + 4 + u64::from(slot % 16) * 2, i)
                .expect("ring");
            self.published[usize::from(queue)] = slot + 1;
        }
        mem.write_u16(avail + 2, self.published[usize::from(queue)])
            .expect("idx");
    }

    fn kick(&mut self, queue: u16) {
        let (desc, avail, used) = ring(queue);
        self.dev.notify(QueueNotify {
            index: queue,
            size: 16,
            desc,
            avail,
            used,
            features: 0,
        });
    }

    fn used(&self, queue: u16, slot: u16) -> (u32, u32) {
        let (_, _, used) = ring(queue);
        let mem = self.memory.lock().expect("mem");
        let at = used + 4 + u64::from(slot) * 8;
        (
            mem.read_u32(at).expect("id"),
            mem.read_u32(at + 4).expect("len"),
        )
    }

    fn used_idx(&self, queue: u16) -> u16 {
        let (_, _, used) = ring(queue);
        self.memory
            .lock()
            .expect("mem")
            .read_u16(used + 2)
            .expect("idx")
    }

    fn event(&self, queue: u16, index: u16) -> InputEvent {
        let mut bytes = [0u8; 8];
        let mem = self.memory.lock().expect("mem");
        mem.read_bytes(buf(queue, index), &mut bytes).expect("read");
        InputEvent::from_bytes(bytes)
    }
}

fn buf(queue: u16, index: u16) -> u64 {
    BUF + u64::from(queue) * 0x800 + u64::from(index) * 8
}

fn syn() -> InputEvent {
    InputEvent::syn()
}

#[test]
fn mmio_reports_identity_and_config_space() {
    let fx = Fx::new(InputKind::Tablet);
    let mut bus = MmioBus::new();
    VirtioMmio::register(&mut bus, 2, Box::new(fx.dev)).expect("register");
    let regs = Regs(RefCell::new([0; 31]));
    let base = ternvale_vmm::VIRTIO_MMIO_BASE + 2 * ternvale_vmm::VIRTIO_MMIO_SLOT_SIZE;
    let access = |offset: u64, size: u8, write: Option<u64>| {
        if let Some(value) = write {
            regs.0.borrow_mut()[1] = value;
        }
        bus.dispatch(
            &regs,
            ExitEvent::Mmio {
                gpa: base + offset,
                size,
                write: write.is_some(),
                reg: if write.is_some() { 1 } else { 0 },
            },
        )
        .expect("dispatch");
        regs.0.borrow()[0]
    };
    let select = |sel: u64, sub: u64| {
        access(0x100, 1, Some(sel));
        access(0x101, 1, Some(sub));
        access(0x102, 1, None)
    };
    assert_eq!(access(0x000, 4, None), 0x7472_6976, "magic");
    assert_eq!(access(0x008, 4, None), u64::from(VIRTIO_INPUT_ID));
    access(0x030, 4, Some(1));
    assert_eq!(access(0x034, 4, None), 64, "statusq max size");

    let name = InputKind::Tablet.name();
    assert_eq!(select(u64::from(CFG_ID_NAME), 0), name.len() as u64);
    let first = access(0x108, 4, None) as u32;
    assert_eq!(&first.to_le_bytes(), &name.as_bytes()[..4]);

    assert_eq!(select(u64::from(CFG_ID_DEVIDS), 0), 8);
    assert_eq!(access(0x108, 4, None), 0x0627_0006, "BUS_VIRTUAL, vendor");
    assert_eq!(access(0x10c, 4, None), 0x0001_0003, "product, version");

    assert_eq!(select(u64::from(CFG_EV_BITS), u64::from(EV_KEY)), 35);
    assert_eq!(
        access(0x108 + 34, 1, None),
        0b0000_0111,
        "BTN_LEFT..BTN_MIDDLE"
    );
    assert_eq!(select(u64::from(CFG_EV_BITS), u64::from(EV_ABS)), 1);
    assert_eq!(access(0x108, 1, None), 0b11, "ABS_X, ABS_Y");
    assert_eq!(select(u64::from(CFG_EV_BITS), u64::from(EV_REL)), 2);
    assert_eq!(access(0x109, 1, None), 0b1, "REL_WHEEL (bit 8)");
    assert_eq!(select(u64::from(CFG_EV_BITS), u64::from(EV_REP)), 0);

    assert_eq!(select(u64::from(CFG_ABS_INFO), u64::from(ABS_Y)), 20);
    assert_eq!(access(0x108, 4, None), 0, "min");
    assert_eq!(access(0x10c, 4, None), u64::from(ABS_MAX_VALUE), "max");
    assert_eq!(select(u64::from(CFG_ABS_INFO), 2), 0, "no ABS_Z");
    assert_eq!(select(u64::from(CFG_PROP_BITS), 0), 0);
    assert_eq!(select(u64::from(CFG_UNSET), 0), 0);
}

#[test]
fn keyboard_config_advertises_keys_and_repeat() {
    let mut fx = Fx::new(InputKind::Keyboard);
    let mut select = |sel: u8, sub: u8| {
        fx.dev
            .write_config(0, 2, u64::from(sel) | u64::from(sub) << 8);
        let size = fx.dev.read_config(2, 1) as usize;
        let bytes: Vec<u8> = (0..size as u64)
            .map(|i| fx.dev.read_config(8 + i, 1) as u8)
            .collect();
        bytes
    };
    let keys = select(CFG_EV_BITS, EV_KEY as u8);
    assert!(keys.len() <= 32, "fits in 128 bytes");
    let has = |code: u16| keys[usize::from(code / 8)] & (1 << (code % 8)) != 0;
    assert!(
        has(30) && has(28) && has(125),
        "KEY_A, KEY_ENTER, KEY_LEFTMETA"
    );
    assert!(!has(0), "KEY_RESERVED");
    assert!(!select(CFG_EV_BITS, EV_REP as u8).is_empty(), "EV_REP");
    assert!(select(CFG_EV_BITS, EV_ABS as u8).is_empty());
    assert!(select(CFG_ABS_INFO, 0).is_empty());
    assert_eq!(select(CFG_ID_SERIAL, 0), b"ternvale-keyboard");
}

#[test]
fn tablet_report_lands_in_eventq_with_syn() {
    let mut fx = Fx::new(InputKind::Tablet);
    fx.post(0, 0, 8, WRITE);
    fx.kick(0);
    assert!(fx.sink.move_to(100, 40_000));
    assert!(fx.sink.key(BTN_LEFT, true));
    assert!(fx.sink.scroll(2, -1));
    let want = [
        InputEvent::abs(ABS_X, 100),
        InputEvent::abs(ABS_Y, ABS_MAX_VALUE),
        syn(),
        InputEvent::key(BTN_LEFT, true),
        syn(),
        InputEvent::rel(REL_WHEEL, -1),
        InputEvent::rel(REL_HWHEEL, 2),
        syn(),
    ];
    assert_eq!(fx.used_idx(0), 8);
    for (i, event) in want.iter().enumerate() {
        assert_eq!(fx.used(0, i as u16), (i as u32, 8));
        assert_eq!(fx.event(0, i as u16), *event, "event {i}");
    }
    assert!(fx.raised.load(Ordering::Acquire), "used-buffer interrupt");
    assert_eq!(fx.sink.stats().snapshot(), [8, 0, 0]);
    assert!(fx.sink.scroll(0, 0), "empty scroll is not a report");
    assert_eq!(fx.sink.pending(), 0);
}

#[test]
fn reports_before_driver_ready_are_dropped() {
    let fx = Fx::new(InputKind::Keyboard);
    assert!(!fx.sink.key(30, true));
    assert_eq!(fx.sink.stats().snapshot(), [0, 2, 0]);
    assert_eq!(fx.sink.pending(), 0);
}

#[test]
fn backlog_waits_for_buffers_then_drops_when_full() {
    let mut fx = Fx::new(InputKind::Keyboard);
    fx.kick(0);
    let fits = MAX_PENDING / 2;
    for i in 0..fits {
        assert!(fx.sink.key(30, i % 2 == 0), "report {i} queued");
    }
    assert!(!fx.sink.key(31, true), "queue full");
    assert!(!fx.sink.key(31, false), "still full");
    assert_eq!(fx.sink.pending(), MAX_PENDING);
    assert_eq!(fx.sink.stats().snapshot(), [0, 4, 0]);

    fx.post(0, 0, 16, WRITE);
    fx.kick(0);
    assert_eq!(fx.used_idx(0), 16);
    assert_eq!(fx.event(0, 0), InputEvent::key(30, true));
    assert_eq!(fx.event(0, 1), syn());
    assert_eq!(fx.event(0, 2), InputEvent::key(30, false));
    assert_eq!(fx.sink.pending(), MAX_PENDING - 16);
    assert!(fx.sink.key(32, true), "room again");
}

#[test]
fn unsupported_events_are_rejected() {
    let mut fx = Fx::new(InputKind::Keyboard);
    fx.post(0, 0, 4, WRITE);
    fx.kick(0);
    assert!(!fx.sink.report(&[InputEvent::abs(ABS_X, 1)]));
    assert!(!fx.sink.key(BTN_LEFT, true), "keyboard has no buttons");
    assert!(!fx.sink.key(0, true), "KEY_RESERVED");
    assert_eq!(fx.used_idx(0), 0);
}

#[test]
fn malformed_event_buffer_completes_empty() {
    let mut fx = Fx::new(InputKind::Keyboard);
    fx.post(0, 0, 1, 0);
    fx.post(0, 1, 2, WRITE);
    fx.kick(0);
    assert!(fx.sink.key(30, true));
    assert_eq!(fx.used(0, 0), (0, 0), "device-readable buffer");
    assert_eq!(fx.used(0, 1), (1, 8));
    assert_eq!(fx.event(0, 1), InputEvent::key(30, true));
    assert_eq!(fx.event(0, 2), syn());
}

#[test]
fn status_queue_events_are_consumed() {
    let mut fx = Fx::new(InputKind::Keyboard);
    {
        let mut mem = fx.memory.lock().expect("mem");
        let led = InputEvent {
            ev_type: 0x11,
            code: 1,
            value: 1,
        };
        mem.write_bytes(buf(1, 0), &led.to_bytes()).expect("led");
    }
    fx.post(1, 0, 1, 0);
    fx.post(1, 1, 1, WRITE);
    fx.kick(1);
    assert_eq!(fx.used(1, 0), (0, 0));
    assert_eq!(fx.used(1, 1), (1, 0), "bad layout still completed");
    assert_eq!(fx.sink.stats().snapshot(), [0, 0, 1]);
}

#[test]
fn reset_unbinds_eventq_and_discards_backlog() {
    let mut fx = Fx::new(InputKind::Keyboard);
    fx.kick(0);
    assert!(fx.sink.key(30, true));
    assert_eq!(fx.sink.pending(), 2);
    fx.dev.status_changed(0);
    assert_eq!(fx.sink.pending(), 0);
    assert!(!fx.sink.key(30, false), "driver not ready after reset");
}

#[test]
fn sink_does_not_keep_the_device_or_memory_alive() {
    let mut fx = Fx::new(InputKind::Tablet);
    fx.post(0, 0, 4, WRITE);
    fx.kick(0);
    let Fx {
        memory, dev, sink, ..
    } = fx;
    drop(dev);
    assert_eq!(Arc::strong_count(&memory), 1, "only the test holds memory");
    assert!(!sink.move_to(1, 1), "device gone");
    assert_eq!(sink.pending(), 0);
    assert_eq!(sink.stats().snapshot(), [0, 3, 0]);
}

#[test]
fn mac_keycodes_map_to_linux_keys() {
    assert_eq!(mac_to_linux_key(0x00), Some(30), "A");
    assert_eq!(mac_to_linux_key(0x24), Some(28), "Return");
    assert_eq!(mac_to_linux_key(0x33), Some(14), "Delete is backspace");
    assert_eq!(mac_to_linux_key(0x75), Some(111), "Forward Delete");
    assert_eq!(mac_to_linux_key(0x7e), Some(103), "Up");
    assert_eq!(mac_to_linux_key(0x3f), None, "fn");
    let mut seen = std::collections::HashSet::new();
    for mac in 0..=0x7f_u16 {
        if let Some(code) = mac_to_linux_key(mac) {
            assert!(code > 0 && code <= 0xff, "{mac:#x} -> {code}");
            assert!(seen.insert(code), "{code} mapped twice");
            assert!(InputKind::Keyboard.supports(EV_KEY, code));
        }
    }
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
