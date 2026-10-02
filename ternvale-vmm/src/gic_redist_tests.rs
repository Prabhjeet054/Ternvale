use std::cell::RefCell;
use std::sync::Arc;

use super::{RedistId, RedistMap, REDIST_STRIDE};
use crate::esr::decode;
use crate::mmio::{GuestRegs, MmioBus, MmioError};
use crate::platform::{GIC_REDIST_BASE, GIC_REDIST_SIZE};
use crate::smp::mpidr;

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

fn bus(map: Arc<RedistMap>) -> MmioBus {
    let mut bus = MmioBus::new();
    bus.register(
        GIC_REDIST_BASE,
        GIC_REDIST_SIZE,
        Box::new(RedistId::new(map)),
    )
    .expect("register");
    bus
}

/// Guest load of `1 << sas` bytes into x0 at `gpa`, through the bus.
fn load(bus: &MmioBus, gpa: u64, sas: u64) -> u64 {
    let regs = Regs(RefCell::new([0; 31]));
    // EC data abort from a lower EL, ISV, SAS, SRT=0, WnR=0.
    let syndrome = (0x24 << 26) | (1 << 24) | (sas << 22);
    bus.dispatch(&regs, decode(syndrome, gpa))
        .expect("dispatch");
    let value = regs.0.borrow()[0];
    value
}

#[test]
fn pidr2_reads_as_gicv3_in_both_frames_of_every_redistributor() {
    let bus = bus(RedistMap::new(2));
    for frame in 0..3 {
        let base = GIC_REDIST_BASE + frame * REDIST_STRIDE;
        assert_eq!(load(&bus, base + 0xffe8, 2), 0x30, "rd frame {frame}");
        assert_eq!(load(&bus, base + 0x1_ffe8, 2), 0x30, "sgi frame {frame}");
    }
}

#[test]
fn typer_carries_each_cpu_affinity_and_last_only_on_the_final_frame() {
    let bus = bus(RedistMap::new(4));
    for cpu in 0..4u64 {
        let typer = load(&bus, GIC_REDIST_BASE + cpu * REDIST_STRIDE + 8, 3);
        assert_eq!(typer >> 32, cpu, "affinity of cpu {cpu}");
        assert_eq!((typer >> 8) & 0xffff, cpu, "processor number of cpu {cpu}");
        assert_eq!(typer & (1 << 4) != 0, cpu == 3, "last bit of cpu {cpu}");
    }
    assert_eq!(load(&bus, GIC_REDIST_BASE + 4 * REDIST_STRIDE + 8, 3), 0);
    let high = load(&bus, GIC_REDIST_BASE + 2 * REDIST_STRIDE + 0xc, 2);
    assert_eq!(high, 2, "32-bit read of the upper half");
}

#[test]
fn single_cpu_typer_matches_the_old_cpu0_value() {
    let bus = bus(RedistMap::new(1));
    assert_eq!(load(&bus, GIC_REDIST_BASE + 8, 3), 1 << 4);
}

#[test]
fn a_framework_base_moves_the_cpu_to_that_frame() {
    let map = RedistMap::new(2);
    map.record(1, mpidr(1), GIC_REDIST_BASE + REDIST_STRIDE);
    assert_eq!(map.typer(1) >> 32, 1, "matching base keeps the default");
    map.record(1, mpidr(1), GIC_REDIST_BASE + 5 * REDIST_STRIDE);
    assert_eq!(map.typer(1), 0, "old frame is empty");
    let moved = map.typer(5);
    assert_eq!(moved >> 32, 1);
    assert_ne!(moved & (1 << 4), 0, "frame 5 is now the last");
    assert_eq!(map.typer(0) & (1 << 4), 0);
    map.record(0, mpidr(0), GIC_REDIST_BASE + 3);
    assert_eq!(map.typer(0) >> 32, 0, "misaligned base is ignored");
}
