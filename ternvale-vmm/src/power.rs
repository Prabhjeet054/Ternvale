//! Host-visible power button: PL061 GPIO (DT `gpio-keys`) and ACPI GED event
//! register. Pressing the button raises both SPIs; vCPU threads must not block
//! on this path.

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;

use crate::mmio::MmioDevice;

/// PL061 register block (PrimeCell). Platform reservation is one host page.
pub const PL061_REG_SIZE: u64 = 0x1000;
/// GED event register window.
pub const GED_REG_SIZE: u64 = 0x1000;
/// gpio-keys KEY_POWER line (QEMU virt gpio-key poweroff).
pub const POWER_GPIO_PIN: u8 = 3;
/// `linux,code` for KEY_POWER.
pub const KEY_POWER: u32 = 116;

const DIR: u64 = 0x400;
pub(super) const IE: u64 = 0x410;
const RIS: u64 = 0x414;
pub(super) const MIS: u64 = 0x418;
const IC: u64 = 0x41c;
const ID_BASE: u64 = 0xfe0;
/// PL061 peripheral + PrimeCell IDs (`gpio-pl061`).
const IDS: [u32; 8] = [0x10, 0x00, 0x04, 0x00, 0x0d, 0xf0, 0x05, 0xb1];

type IrqHook = Arc<dyn Fn(bool) + Send + Sync>;

/// Shared pin/event state for PL061 + GED.
pub struct PowerButton {
    /// Input bits; pin 3 defaults high (gpio-keys `GPIO_ACTIVE_LOW`).
    pins: AtomicU8,
    dir: AtomicU8,
    ie: AtomicU8,
    ris: AtomicU8,
    ged: AtomicU32,
    irq_gpio: IrqHook,
    irq_ged: IrqHook,
}

impl PowerButton {
    /// Button that drives `irq_gpio` / `irq_ged` (level).
    #[tracing::instrument(level = "debug", target = "ternvale::power", skip_all)]
    pub fn new(irq_gpio: IrqHook, irq_ged: IrqHook) -> Arc<Self> {
        let button = Arc::new(Self {
            pins: AtomicU8::new(1 << POWER_GPIO_PIN),
            dir: AtomicU8::new(0),
            ie: AtomicU8::new(0),
            ris: AtomicU8::new(0),
            ged: AtomicU32::new(0),
            irq_gpio,
            irq_ged,
        });
        tracing::info!(target: "ternvale::power", pin = POWER_GPIO_PIN, "power button ready");
        button
    }

    /// Pulse ACPI GED bit 0 and drive GPIO pin 3 low (KEY_POWER).
    #[tracing::instrument(level = "debug", target = "ternvale::power", skip_all)]
    pub fn press(&self) {
        tracing::info!(
            target: "ternvale::power",
            pin = POWER_GPIO_PIN,
            "power button pressed"
        );
        self.pins
            .fetch_and(!(1 << POWER_GPIO_PIN), Ordering::SeqCst);
        self.ris.fetch_or(1 << POWER_GPIO_PIN, Ordering::SeqCst);
        self.ged.fetch_or(1, Ordering::SeqCst);
        self.raise_gpio();
        (self.irq_ged)(true);
    }

    fn raise_gpio(&self) {
        let pending = self.ris.load(Ordering::SeqCst) & self.ie.load(Ordering::SeqCst);
        (self.irq_gpio)(pending != 0);
    }
}

/// ARM PL061 GPIO (inputs for gpio-keys).
pub struct Pl061 {
    button: Arc<PowerButton>,
}

impl Pl061 {
    /// GPIO window for `button`.
    #[tracing::instrument(level = "debug", target = "ternvale::power", skip_all)]
    pub fn new(button: Arc<PowerButton>) -> Self {
        Self { button }
    }
}

impl MmioDevice for Pl061 {
    fn name(&self) -> &str {
        "pl061"
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        if size != 1 && size != 4 {
            tracing::warn!(
                target: "ternvale::power",
                offset = format!("{offset:#x}"),
                size,
                "pl061 rejected read size"
            );
            return 0;
        }
        let word = if offset < 0x400 {
            let mask = (offset >> 2) as u8;
            let mask = if mask == 0 { 0xff } else { mask };
            u32::from(self.button.pins.load(Ordering::SeqCst) & mask)
        } else if (ID_BASE..ID_BASE + 32).contains(&offset) {
            let index = ((offset - ID_BASE) / 4) as usize;
            IDS.get(index).copied().unwrap_or(0)
        } else {
            match offset {
                DIR => u32::from(self.button.dir.load(Ordering::SeqCst)),
                IE => u32::from(self.button.ie.load(Ordering::SeqCst)),
                RIS => u32::from(self.button.ris.load(Ordering::SeqCst)),
                MIS => u32::from(
                    self.button.ris.load(Ordering::SeqCst) & self.button.ie.load(Ordering::SeqCst),
                ),
                IC => 0,
                _ => 0,
            }
        };
        u64::from(word)
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        if size != 1 && size != 4 {
            tracing::warn!(
                target: "ternvale::power",
                offset = format!("{offset:#x}"),
                size,
                "pl061 rejected write size"
            );
            return;
        }
        let byte = val as u8;
        if offset < 0x400 {
            let mask = (offset >> 2) as u8;
            let mask = if mask == 0 { 0xff } else { mask };
            let dir = self.button.dir.load(Ordering::SeqCst);
            let out = mask & dir;
            if out == 0 {
                return;
            }
            let bits = self.button.pins.load(Ordering::SeqCst);
            self.button
                .pins
                .store((bits & !out) | (byte & out), Ordering::SeqCst);
            return;
        }
        match offset {
            DIR => self.button.dir.store(byte, Ordering::SeqCst),
            IE => {
                self.button.ie.store(byte, Ordering::SeqCst);
                self.button.raise_gpio();
            }
            IC => {
                self.button.ris.fetch_and(!byte, Ordering::SeqCst);
                self.button.raise_gpio();
            }
            _ => {}
        }
    }
}

/// Memory-mapped GED event register: bit 0 is the power button; writes are W1C.
pub struct GedRegs {
    button: Arc<PowerButton>,
}

impl GedRegs {
    /// Event window for `button`.
    #[tracing::instrument(level = "debug", target = "ternvale::power", skip_all)]
    pub fn new(button: Arc<PowerButton>) -> Self {
        Self { button }
    }
}

impl MmioDevice for GedRegs {
    fn name(&self) -> &str {
        "ged"
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        if offset != 0 || (size != 4 && size != 1) {
            tracing::warn!(
                target: "ternvale::power",
                offset = format!("{offset:#x}"),
                size,
                "ged rejected read"
            );
            return 0;
        }
        u64::from(self.button.ged.load(Ordering::SeqCst))
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        if offset != 0 || (size != 4 && size != 1) {
            tracing::warn!(
                target: "ternvale::power",
                offset = format!("{offset:#x}"),
                size,
                "ged rejected write"
            );
            return;
        }
        let prev = self.button.ged.fetch_and(!(val as u32), Ordering::SeqCst);
        let next = prev & !(val as u32);
        if next == 0 {
            (self.button.irq_ged)(false);
        }
        tracing::debug!(
            target: "ternvale::power",
            prev,
            next,
            "ged event register write-1-clear"
        );
    }
}

#[cfg(test)]
#[path = "power_tests.rs"]
mod tests;
