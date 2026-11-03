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
    assert!(text.contains("unmapped mmio access"), "{text}");
    assert!(text.contains("mmio access count"), "{text}");
    assert!(text.contains("accesses=1"), "{text}");
    // TRACE lines exist only when compiled in (debug, or release `trace-hot`
    // without `quiet-trace`). `contains("mmio access")` also matches WARN.
    let trace_compiled = cfg!(any(
        debug_assertions,
        all(feature = "trace-hot", not(feature = "quiet-trace"))
    ));
    if trace_compiled {
        assert!(text.contains("device=\"counter\""), "{text}");
        assert!(text.contains("offset=\"0x4\""), "{text}");
        assert!(text.contains("direction=\"write\""), "{text}");
    } else {
        assert!(
            !text.contains("offset=\"0x4\""),
            "quiet-trace release must not emit MMIO TRACE: {text}"
        );
        assert!(!text.contains("direction=\"write\""), "{text}");
    }
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

#[test]
fn reverse_registration_still_dispatches() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut bus = MmioBus::new();
    bus.register(
        0x2000,
        0x100,
        Box::new(recording("high", Arc::clone(&writes))),
    )
    .expect("high");
    bus.register(
        0x1000,
        0x100,
        Box::new(recording("low", Arc::clone(&writes))),
    )
    .expect("low");
    let regs = MockRegs {
        regs: RefCell::new([0; 31]),
    };
    regs.regs.borrow_mut()[1] = 0x11;
    bus.dispatch(&regs, mmio(0x1000, 1, true, 1)).expect("low");
    regs.regs.borrow_mut()[1] = 0x22;
    bus.dispatch(&regs, mmio(0x2000, 1, true, 1)).expect("high");
    regs.regs.borrow_mut()[1] = 0x33;
    bus.dispatch(&regs, mmio(0x1000, 1, true, 1))
        .expect("low hit");
    assert_eq!(
        writes.lock().expect("writes").as_slice(),
        &[(0, 1, 0x11), (0, 1, 0x22), (0, 1, 0x33)]
    );
}

/// Release `quiet-trace` compiles TRACE out; `trace-hot` without `quiet-trace` keeps it.
#[test]
fn static_max_level_matches_trace_features() {
    use tracing::level_filters::{LevelFilter, STATIC_MAX_LEVEL};
    assert!(
        STATIC_MAX_LEVEL >= LevelFilter::DEBUG,
        "DEBUG must stay compiled in"
    );
    #[cfg(all(not(debug_assertions), feature = "quiet-trace"))]
    assert!(
        STATIC_MAX_LEVEL < LevelFilter::TRACE,
        "quiet-trace release must compile TRACE out, got {STATIC_MAX_LEVEL:?}"
    );
    #[cfg(all(
        not(debug_assertions),
        feature = "trace-hot",
        not(feature = "quiet-trace")
    ))]
    assert_eq!(
        STATIC_MAX_LEVEL,
        LevelFilter::TRACE,
        "trace-hot release without quiet-trace must keep TRACE"
    );
}

/// Mean ns per `dispatch` for `scripts/bench` / `compare.py` when
/// `TERNVALE_MMIO_BENCH` is a destination path. No-op in ordinary `cargo test`.
#[test]
fn write_dispatch_timing_when_env_set() {
    let Ok(path) = std::env::var("TERNVALE_MMIO_BENCH") else {
        return;
    };
    let mut bus = MmioBus::new();
    for i in 0..32u64 {
        bus.register(0x1000 + i * 0x100, 0x100, Box::new(dev("probe")))
            .expect("register");
    }
    let regs = MockRegs {
        regs: RefCell::new([0; 31]),
    };
    let event = mmio(0x1000 + 31 * 0x100, 1, false, 0);
    for _ in 0..50 {
        bus.dispatch(&regs, event).expect("warmup");
    }
    let samples = 200u32;
    let started = std::time::Instant::now();
    for _ in 0..samples {
        bus.dispatch(&regs, event).expect("timed");
    }
    let ns = started.elapsed().as_nanos() / u128::from(samples);
    tracing::info!(
        target: "ternvale::mmio",
        samples,
        ns_per_dispatch = ns as u64,
        path = %path,
        "mmio dispatch timing for compare.py"
    );
    let body = format!("{{\"runs\":{{\"ternvale\":{{\"mmio_dispatch_ns\":{ns}}}}}}}\n");
    std::fs::write(&path, body).expect("write mmio bench json");
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
