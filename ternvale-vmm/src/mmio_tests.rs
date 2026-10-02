use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use super::{GuestRegs, MmioBus, MmioDevice, MmioError};
use crate::esr::ExitEvent;

struct MockRegs {
    regs: RefCell<[u64; 31]>,
}

impl GuestRegs for MockRegs {
    fn get_reg(&self, index: u8) -> Result<u64, MmioError> {
        Ok(self.regs.borrow()[usize::from(index)])
    }

    fn set_reg(&self, index: u8, value: u64) -> Result<(), MmioError> {
        self.regs.borrow_mut()[usize::from(index)] = value;
        Ok(())
    }
}

struct MockDev {
    label: &'static str,
    next_read: u64,
    writes: Arc<Mutex<Vec<(u64, u8, u64)>>>,
}

impl MmioDevice for MockDev {
    fn name(&self) -> &str {
        self.label
    }

    fn read(&mut self, _offset: u64, _size: u8) -> u64 {
        self.next_read
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        self.writes
            .lock()
            .expect("writes")
            .push((offset, size, val));
    }
}

fn mmio(gpa: u64, size: u8, write: bool, reg: u8) -> ExitEvent {
    ExitEvent::Mmio {
        gpa,
        size,
        write,
        reg,
    }
}

#[test]
fn rejects_overlap_and_accepts_adjacent_ranges() {
    let mut bus = MmioBus::new();
    bus.register(0x1000, 0x100, Box::new(dev("a")))
        .expect("first");
    let overlap = bus.register(0x1080, 0x100, Box::new(dev("b"))).unwrap_err();
    assert!(overlap.to_string().contains("overlaps"), "{overlap}");
    bus.register(0x1100, 0x100, Box::new(dev("c")))
        .expect("adjacent");
    let empty = bus.register(0x2000, 0, Box::new(dev("d"))).unwrap_err();
    assert!(matches!(empty, MmioError::Empty { base: 0x2000 }));
}

#[test]
fn read_and_write_hit_the_first_and_last_byte() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut bus = MmioBus::new();
    bus.register(
        0x1000,
        0x100,
        Box::new(recording("uart", Arc::clone(&writes))),
    )
    .expect("reg");
    let regs = MockRegs {
        regs: RefCell::new([0; 31]),
    };
    regs.regs.borrow_mut()[3] = 0xaa;
    bus.dispatch(&regs, mmio(0x1000, 1, true, 3))
        .expect("first");
    regs.regs.borrow_mut()[4] = 0xbb;
    bus.dispatch(&regs, mmio(0x10ff, 1, true, 4)).expect("last");
    bus.dispatch(&regs, mmio(0x1100, 1, true, 4))
        .expect("past the window");
    assert_eq!(regs.regs.borrow()[4], 0xbb);
    assert_eq!(
        writes.lock().expect("writes").as_slice(),
        &[(0, 1, 0xaa), (0xff, 1, 0xbb)]
    );
}

#[test]
fn unmapped_read_is_zero_and_unmapped_write_is_ignored() {
    let bus = MmioBus::new();
    let regs = MockRegs {
        regs: RefCell::new([0; 31]),
    };
    regs.regs.borrow_mut()[1] = 0x1234;
    bus.dispatch(&regs, mmio(0x0900_0000, 1, false, 1))
        .expect("read");
    assert_eq!(regs.regs.borrow()[1], 0);
    regs.regs.borrow_mut()[1] = 0x1234;
    bus.dispatch(&regs, mmio(0x0900_0000, 1, true, 1))
        .expect("write");
    assert_eq!(regs.regs.borrow()[1], 0x1234);
    assert_eq!(bus.access_count("uart"), None);
}

#[test]
fn sizes_mask_the_register_and_reads_write_back() {
    let mut bus = MmioBus::new();
    bus.register(
        0x2000,
        0x100,
        Box::new(MockDev {
            label: "dev",
            next_read: 0x1122_3344_5566_7788,
            writes: Arc::new(Mutex::new(Vec::new())),
        }),
    )
    .expect("reg");
    let regs = MockRegs {
        regs: RefCell::new([0; 31]),
    };
    for (size, reg) in [(1u8, 0u8), (2, 1), (4, 2), (8, 3)] {
        regs.regs.borrow_mut()[usize::from(reg)] = u64::MAX;
        bus.dispatch(&regs, mmio(0x2000, size, true, reg))
            .expect("write");
        bus.dispatch(&regs, mmio(0x2000, size, false, reg))
            .expect("read");
    }
    assert_eq!(regs.regs.borrow()[0], 0x88);
    assert_eq!(regs.regs.borrow()[1], 0x7788);
    assert_eq!(regs.regs.borrow()[2], 0x5566_7788);
    assert_eq!(regs.regs.borrow()[3], 0x1122_3344_5566_7788);
    assert_eq!(bus.access_count("dev"), Some(8));
}

#[test]
fn shutdown_logs_the_access_count() {
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-mmio", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let mut config = ternvale_log::LogConfig::new("mmio", dir.clone());
    config.level = "trace".to_string();
    let guard = ternvale_log::init(config).expect("log");
    let mut bus = MmioBus::new();
    bus.register(0x3000, 0x10, Box::new(dev("counter")))
        .expect("reg");
    let regs = MockRegs {
        regs: RefCell::new([0; 31]),
    };
    regs.regs.borrow_mut()[0] = 0x7;
    bus.dispatch(&regs, mmio(0x3004, 1, true, 0))
        .expect("write");
    bus.dispatch(&regs, mmio(0x4000, 1, false, 0))
        .expect("unmapped");
    drop(bus);
    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    assert!(text.contains("mmio access"), "{text}");
    assert!(text.contains("device=\"counter\""), "{text}");
    assert!(text.contains("offset=\"0x4\""), "{text}");
    assert!(text.contains("direction=\"write\""), "{text}");
    assert!(text.contains("unmapped mmio access"), "{text}");
    assert!(text.contains("mmio access count"), "{text}");
    assert!(text.contains("accesses=1"), "{text}");
    std::fs::remove_dir_all(&dir).expect("remove log dir");
}

#[test]
fn concurrent_vcpu_threads_share_one_bus_and_lose_no_access() {
    const THREADS: u64 = 4;
    const PER_THREAD: u64 = 500;
    let a = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::new(Mutex::new(Vec::new()));
    let mut bus = MmioBus::new();
    bus.register(0x1000, 0x100, Box::new(recording("a", Arc::clone(&a))))
        .expect("a");
    bus.register(0x2000, 0x100, Box::new(recording("b", Arc::clone(&b))))
        .expect("b");
    let bus = &bus;
    std::thread::scope(|scope| {
        for thread in 0..THREADS {
            scope.spawn(move || {
                let regs = MockRegs {
                    regs: RefCell::new([0; 31]),
                };
                regs.regs.borrow_mut()[0] = thread;
                let gpa = if thread % 2 == 0 { 0x1000 } else { 0x2000 };
                for _ in 0..PER_THREAD {
                    bus.dispatch(&regs, mmio(gpa + thread, 1, true, 0))
                        .expect("write");
                }
            });
        }
    });
    let total = THREADS / 2 * PER_THREAD;
    assert_eq!(bus.access_count("a"), Some(total));
    assert_eq!(bus.access_count("b"), Some(total));
    let a = a.lock().expect("a");
    assert_eq!(a.len() as u64, total);
    assert!(a
        .iter()
        .all(|(offset, _, val)| offset == val && val % 2 == 0));
    let b = b.lock().expect("b");
    assert!(b
        .iter()
        .all(|(offset, _, val)| offset == val && val % 2 == 1));
}

fn dev(label: &'static str) -> MockDev {
    recording(label, Arc::new(Mutex::new(Vec::new())))
}

fn recording(label: &'static str, writes: Arc<Mutex<Vec<(u64, u8, u64)>>>) -> MockDev {
    MockDev {
        label,
        next_read: 0,
        writes,
    }
}
