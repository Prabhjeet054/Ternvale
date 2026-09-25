//! GICv3 redistributor accesses the in-kernel GIC does not answer on its own.
//!
//! Linux checks `GICR_PIDR2` at offset `0xFFE8` before it trusts `GICR_TYPER`.
//! Apple's framework leaves that ID for the VMM. Other redistributor writes are
//! dropped: `hv_gic_set_redistributor_reg` returns `HV_DENIED` once the guest runs.

use crate::mmio::MmioDevice;

/// `GICR_PIDR2.ArchRev` value for GICv3.
const PIDR2_GICV3: u64 = 0x30;
const PIDR2_OFFSET: u64 = 0xffe8;
/// `GICR_TYPER` for CPU 0 with the Last redistributor bit set.
const TYPER_LAST_CPU0: u64 = 1 << 4;
const TYPER_OFFSET: u64 = 0x8;

/// Redistributor identification registers.
pub struct RedistId;

impl MmioDevice for RedistId {
    fn name(&self) -> &str {
        "gic-redist"
    }

    fn read(&mut self, offset: u64, _size: u8) -> u64 {
        let frame = offset & 0xffff;
        let value = if frame == PIDR2_OFFSET {
            PIDR2_GICV3
        } else if offset < 0x1_0000 && frame == TYPER_OFFSET {
            TYPER_LAST_CPU0
        } else {
            0
        };
        if value != 0 {
            tracing::trace!(
                target: "ternvale::gic",
                offset = format!("{:#x}", offset),
                value = format!("{:#x}", value),
                "gic redistributor read"
            );
            return value;
        }
        tracing::debug!(
            target: "ternvale::gic",
            offset = format!("{:#x}", offset),
            "gic redistributor read not claimed by the framework"
        );
        0
    }

    fn write(&mut self, offset: u64, _size: u8, value: u64) {
        tracing::debug!(
            target: "ternvale::gic",
            offset = format!("{:#x}", offset),
            value = format!("{:#x}", value),
            "gic redistributor write not claimed by the framework"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::RedistId;
    use crate::esr::decode;
    use crate::mmio::{GuestRegs, MmioBus, MmioError};
    use crate::platform::{GIC_REDIST_BASE, GIC_REDIST_SIZE};
    use std::cell::RefCell;

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

    #[test]
    fn pidr2_reads_as_gicv3() {
        let mut bus = MmioBus::new();
        bus.register(GIC_REDIST_BASE, GIC_REDIST_SIZE, Box::new(RedistId))
            .expect("register");
        let regs = Regs(RefCell::new([0; 31]));
        // ISV, SAS=4, SRT=0, WnR=0. EC data abort lower EL.
        let syndrome = (0x24 << 26) | (1 << 24) | (2 << 22);
        let event = decode(syndrome, GIC_REDIST_BASE + 0xffe8);
        bus.dispatch(&regs, event).expect("dispatch");
        assert_eq!(regs.0.borrow()[0], 0x30);
    }
}
