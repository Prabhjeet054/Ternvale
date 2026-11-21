//! QEMU pvpanic MMIO (`qemu,pvpanic-mmio`) and the shared event byte.
//!
//! Identifiers: PCI vendor `0x1b36` device `0x0011` (QEMU `pvpanic-pci`);
//! ACPI `_HID` `QEMU0001` (`drivers/misc/pvpanic/pvpanic-acpi.c`);
//! DT `compatible = "qemu,pvpanic-mmio"`.
//! Event bits: Linux `include/uapi/linux/pvpanic.h` / QEMU `hw/misc/pvpanic.h`.
//! Windows: virtio-win `pvpanic` binds the same PCI ID (`1b36:0011`);
//! TODO(verify) the ARM64 virtio-win package still ships that INF.

use std::sync::Arc;

use crate::crash::{CrashHub, CrashKind, PVPANIC_EVENTS};
use crate::mmio::MmioDevice;

/// MMIO register block (QEMU `VIRT_PVPANIC` size is 2).
pub const PVPANIC_MMIO_LEN: u64 = 2;

/// Platform pvpanic.
pub struct PvPanicMmio {
    hub: Arc<CrashHub>,
}

impl PvPanicMmio {
    /// Events go to `hub`.
    #[tracing::instrument(level = "debug", target = "ternvale::pvpanic", skip_all)]
    pub fn new(hub: Arc<CrashHub>) -> Self {
        Self { hub }
    }
}

impl MmioDevice for PvPanicMmio {
    fn name(&self) -> &str {
        "pvpanic"
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        if offset > 1 {
            tracing::warn!(
                target: "ternvale::pvpanic",
                offset = format!("{offset:#x}"),
                size,
                "pvpanic rejected read"
            );
            return 0;
        }
        u64::from(PVPANIC_EVENTS)
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        if offset != 0 && offset != 1 {
            tracing::warn!(
                target: "ternvale::pvpanic",
                offset = format!("{offset:#x}"),
                size,
                value = format!("{val:#x}"),
                "pvpanic rejected write"
            );
            return;
        }
        let events = (val as u8) & PVPANIC_EVENTS;
        if events == 0 {
            return;
        }
        self.hub.report(CrashKind::Panic { events });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash::{PVPANIC_PANICKED, PVPANIC_SHUTDOWN};

    #[test]
    fn read_advertises_spec_bits_and_write_queues_before_bind() {
        let hub = CrashHub::new();
        let mut dev = PvPanicMmio::new(Arc::clone(&hub));
        assert_eq!(dev.read(0, 1) as u8, PVPANIC_EVENTS);
        dev.write(0, 1, u64::from(PVPANIC_PANICKED | PVPANIC_SHUTDOWN));
    }

    #[test]
    fn mmio_bus_read_returns_event_bits() {
        let hub = CrashHub::new();
        let mut bus = crate::mmio::MmioBus::new();
        bus.register(0x1000, 2, Box::new(PvPanicMmio::new(hub)))
            .expect("reg");
        struct Regs(std::cell::Cell<u64>);
        impl crate::mmio::GuestRegs for Regs {
            fn get_reg(&self, _: u8) -> Result<u64, crate::mmio::MmioError> {
                Ok(self.0.get())
            }
            fn set_reg(&self, _: u8, value: u64) -> Result<(), crate::mmio::MmioError> {
                self.0.set(value);
                Ok(())
            }
        }
        let regs = Regs(std::cell::Cell::new(0));
        bus.dispatch(
            &regs,
            crate::esr::ExitEvent::Mmio {
                gpa: 0x1000,
                size: 1,
                write: false,
                reg: 0,
            },
        )
        .expect("read");
        assert_eq!(regs.0.get() as u8, PVPANIC_EVENTS);
    }
}
