use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::{Diagnostics, MmioTrace};
use crate::esr::ExitEvent;
use crate::mmio::{GuestRegs, MmioBus, MmioDevice, MmioError};

struct Regs {
    regs: RefCell<[u64; 31]>,
    cpu: Option<u64>,
}

impl GuestRegs for Regs {
    fn get_reg(&self, index: u8) -> Result<u64, MmioError> {
        Ok(self.regs.borrow()[usize::from(index)])
    }

    fn set_reg(&self, index: u8, value: u64) -> Result<(), MmioError> {
        self.regs.borrow_mut()[usize::from(index)] = value;
        Ok(())
    }

    fn cpu_id(&self) -> Option<u64> {
        self.cpu
    }
}

struct Echo;

impl MmioDevice for Echo {
    fn name(&self) -> &str {
        "echo"
    }

    fn read(&mut self, offset: u64, _size: u8) -> u64 {
        0x100 + offset
    }

    fn write(&mut self, _offset: u64, _size: u8, _val: u64) {}
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
fn ring_keeps_the_last_events_in_order() {
    let trace = MmioTrace::new(4);
    let counter = Arc::new(AtomicU64::new(0));
    let uart = trace.add_window("uart", 0x900_0000, 0x1000, Arc::clone(&counter));
    for n in 0..6u64 {
        trace.record(Some(uart), Some(1), n % 2 == 0, 0x900_0000 + n, 4, n);
    }
    assert_eq!(trace.total(), 6);
    let events = trace.events();
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        [2, 3, 4, 5]
    );
    let last = events.last().expect("event");
    assert_eq!(last.device, "uart");
    assert_eq!(last.offset, 5);
    assert_eq!(last.value, 5);
    assert_eq!(last.size, 4);
    assert_eq!(last.cpu, Some(1));
    assert!(!last.write);
    assert!(events[0].write);
}

#[test]
fn unmapped_and_unknown_cpu_are_reported() {
    let trace = MmioTrace::new(8);
    trace.record(None, None, true, 0xdead_0000, 8, 7);
    trace.record(None, Some(u64::MAX), false, 0xdead_0008, 1, 0);
    let events = trace.events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].device, "unmapped");
    assert_eq!(events[0].offset, 0xdead_0000);
    assert_eq!(events[0].cpu, None);
    assert_eq!(events[1].cpu, None, "an id that does not fit 32 bits");
    assert_eq!(trace.unmapped(), 2);
}

#[test]
fn zero_capacity_still_keeps_one_event() {
    let trace = MmioTrace::new(0);
    trace.record(None, Some(0), false, 0x10, 4, 1);
    trace.record(None, Some(0), false, 0x20, 4, 2);
    let events = trace.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].gpa, 0x20);
}

#[test]
fn bus_records_into_the_trace_and_shares_counts() {
    let trace = Arc::new(MmioTrace::new(16));
    let mut bus = MmioBus::with_trace(Arc::clone(&trace));
    bus.register(0x1000, 0x100, Box::new(Echo))
        .expect("register");
    let regs = Regs {
        regs: RefCell::new([0; 31]),
        cpu: Some(3),
    };
    regs.regs.borrow_mut()[2] = 0xab;
    bus.dispatch(&regs, mmio(0x1010, 4, false, 1))
        .expect("read");
    bus.dispatch(&regs, mmio(0x1020, 1, true, 2))
        .expect("write");
    bus.dispatch(&regs, mmio(0x5000, 4, false, 1))
        .expect("unmapped");
    let events = trace.events();
    assert_eq!(events.len(), 3);
    assert_eq!(
        (events[0].device.as_str(), events[0].offset, events[0].value),
        ("echo", 0x10, 0x110)
    );
    assert_eq!((events[1].write, events[1].value), (true, 0xab));
    assert_eq!(events[2].device, "unmapped");
    assert!(events.iter().all(|e| e.cpu == Some(3)));
    let devices = trace.devices();
    assert_eq!(devices.len(), 1);
    assert_eq!(
        (
            devices[0].name.as_str(),
            devices[0].base,
            devices[0].accesses
        ),
        ("echo", 0x1000, 2)
    );
    assert_eq!(bus.access_count("echo"), Some(2));
}

#[test]
fn concurrent_writers_never_produce_torn_events() {
    let trace = Arc::new(MmioTrace::new(64));
    let window = trace.add_window("dev", 0, 1 << 32, Arc::new(AtomicU64::new(0)));
    let done = Arc::new(AtomicU64::new(0));
    let writers: Vec<_> = (0..4u64)
        .map(|cpu| {
            let trace = Arc::clone(&trace);
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                for n in 0..20_000u64 {
                    let tag = (cpu << 24) | n;
                    trace.record(Some(window), Some(cpu), true, tag, 8, !tag);
                }
                done.fetch_add(1, Ordering::SeqCst);
            })
        })
        .collect();
    while done.load(Ordering::SeqCst) < 4 {
        for event in trace.events() {
            assert_eq!(event.value, !event.gpa, "torn event {event:?}");
            assert_eq!(Some((event.gpa >> 24) as u32), event.cpu);
        }
    }
    for writer in writers {
        writer.join().expect("writer");
    }
    assert_eq!(trace.total(), 80_000);
    let kept = trace.events().len() as u64;
    assert!(kept <= 64 && kept + trace.dropped() >= 64, "kept {kept}");
}

#[test]
fn diagnostics_keep_the_first_dtb() {
    let diag = Diagnostics::new();
    assert!(diag.dtb().is_none());
    diag.set_dtb(&[0xd0, 0x0d, 0xfe, 0xed]);
    diag.set_dtb(&[1, 2, 3]);
    assert_eq!(diag.dtb(), Some(&[0xd0, 0x0d, 0xfe, 0xed][..]));
    assert_eq!(diag.mmio().total(), 0);
}
