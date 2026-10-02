use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::{Pl031, PL031_REG_SIZE};
use crate::esr::ExitEvent;
use crate::mmio::{GuestRegs, MmioBus, MmioError};
use crate::platform::RTC_BASE;

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

struct Guest {
    bus: MmioBus,
    regs: Regs,
    clock: Arc<AtomicU64>,
}

impl Guest {
    fn new(start: u64) -> Self {
        let clock = Arc::new(AtomicU64::new(start));
        let source = Arc::clone(&clock);
        let rtc = Pl031::with_clock(Box::new(move || source.load(Ordering::Relaxed)));
        let mut bus = MmioBus::new();
        bus.register(RTC_BASE, PL031_REG_SIZE, Box::new(rtc))
            .expect("register");
        Self {
            bus,
            regs: Regs(RefCell::new([0; 31])),
            clock,
        }
    }

    fn read(&self, offset: u64, size: u8) -> u64 {
        let event = ExitEvent::Mmio {
            gpa: RTC_BASE + offset,
            size,
            write: false,
            reg: 1,
        };
        self.bus.dispatch(&self.regs, event).expect("read");
        self.regs.get_reg(1).expect("x1")
    }

    fn write(&self, offset: u64, value: u64) {
        self.regs.set_reg(2, value).expect("x2");
        let event = ExitEvent::Mmio {
            gpa: RTC_BASE + offset,
            size: 4,
            write: true,
            reg: 2,
        };
        self.bus.dispatch(&self.regs, event).expect("write");
    }
}

#[test]
fn id_bytes_match_what_edk2_checks() {
    let guest = Guest::new(0);
    let periph: Vec<u64> = (0..4).map(|i| guest.read(0xfe0 + 4 * i, 1)).collect();
    let pcell: Vec<u64> = (0..4).map(|i| guest.read(0xff0 + 4 * i, 1)).collect();
    assert_eq!(periph, [0x31, 0x10, 0x14, 0x00]);
    assert_eq!(guest.read(0xfe8, 1) & 0xf, 0x04, "PERIPH_ID2 & 0xf");
    assert_eq!(pcell, [0x0d, 0xf0, 0x05, 0xb1]);
}

#[test]
fn data_register_tracks_the_clock_and_load_sets_an_offset() {
    let guest = Guest::new(1_700_000_000);
    assert_eq!(guest.read(0x00, 4), 1_700_000_000);
    guest.clock.fetch_add(5, Ordering::Relaxed);
    assert_eq!(guest.read(0x00, 4), 1_700_000_005);
    guest.write(0x08, 1_000);
    assert_eq!(guest.read(0x00, 4), 1_000);
    assert_eq!(guest.read(0x08, 4), 1_000, "LR reads back");
    guest.clock.fetch_add(7, Ordering::Relaxed);
    assert_eq!(guest.read(0x00, 4), 1_007);
    assert_eq!(guest.read(0x0c, 4), 1, "CR always enabled");
    guest.write(0x0c, 0);
    assert_eq!(guest.read(0x0c, 4), 1);
}

#[test]
fn match_latches_raw_status_and_mask_gates_mis() {
    let guest = Guest::new(100);
    guest.write(0x10, 1);
    guest.write(0x04, 110);
    assert_eq!(guest.read(0x14, 4), 0, "not yet");
    guest.clock.store(110, Ordering::Relaxed);
    assert_eq!(guest.read(0x14, 4), 1, "RIS latched");
    assert_eq!(guest.read(0x18, 4), 1, "MIS with IMSC set");
    guest.write(0x10, 0);
    assert_eq!(guest.read(0x18, 4), 0, "masked");
    guest.write(0x1c, 1);
    assert_eq!(guest.read(0x14, 4), 0, "ICR clears");
}

#[test]
fn unknown_registers_read_zero() {
    let guest = Guest::new(1);
    assert_eq!(guest.read(0x40, 4), 0);
    guest.write(0x00, 5);
    assert_eq!(guest.read(0x00, 4), 1, "DR is read-only");
}
