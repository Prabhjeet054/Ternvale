//! ARM PL031 real-time clock at [`crate::platform::RTC_BASE`] (QEMU `VIRT_RTC`).
//!
//! EDK2 needs it: the RTC is a UEFI architectural protocol, and
//! `PL031RealTimeClockLib` only accepts the device after reading the PrimeCell
//! and peripheral ID bytes (`0x0d f0 05 b1`, `0x31 10 x4 00`) with byte loads.
//! Linux's `rtc-pl031` uses it as `rtc0`.
//!
//! Model (after QEMU `hw/rtc/pl031.c`): `DR` is host UTC seconds plus an
//! offset that `LR` writes set; `CR` always reads 1. The match interrupt is
//! latched into `RIS` when an access sees `DR >= MR`, but nothing drives the
//! GIC line yet (no host timer); see the known issues in `docs/PROGRESS.md`.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::mmio::MmioDevice;

/// PL031 register block. QEMU's `VIRT_RTC` size.
pub const PL031_REG_SIZE: u64 = 0x1000;
/// GIC SPI, level-high. QEMU's `VIRT_RTC` interrupt.
pub const RTC_SPI: u32 = 2;

const DR: u64 = 0x00;
const MR: u64 = 0x04;
const LR: u64 = 0x08;
const CR: u64 = 0x0c;
const IMSC: u64 = 0x10;
const RIS: u64 = 0x14;
const MIS: u64 = 0x18;
const ICR: u64 = 0x1c;
const ID_BASE: u64 = 0xfe0;
/// `PERIPH_ID0..3` then `PCELL_ID0..3`.
const IDS: [u32; 8] = [0x31, 0x10, 0x14, 0x00, 0x0d, 0xf0, 0x05, 0xb1];

type Clock = Box<dyn Fn() -> u64 + Send>;

/// One PL031.
pub struct Pl031 {
    clock: Clock,
    /// Guest seconds minus host seconds.
    offset: i64,
    load: u32,
    alarm: u32,
    armed: bool,
    mask: u32,
    raw: u32,
}

impl std::fmt::Debug for Pl031 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Pl031")
            .field("offset", &self.offset)
            .field("alarm", &self.alarm)
            .finish()
    }
}

impl Default for Pl031 {
    fn default() -> Self {
        Self::new()
    }
}

impl Pl031 {
    /// An RTC reading host UTC.
    #[tracing::instrument(level = "debug", target = "ternvale::rtc", skip_all)]
    pub fn new() -> Self {
        Self::with_clock(Box::new(host_seconds))
    }

    /// An RTC reading `clock` (seconds since the Unix epoch).
    pub fn with_clock(clock: Clock) -> Self {
        let rtc = Self {
            clock,
            offset: 0,
            load: 0,
            alarm: 0,
            armed: false,
            mask: 0,
            raw: 0,
        };
        tracing::info!(target: "ternvale::rtc", seconds = rtc.now(), "pl031 rtc ready");
        rtc
    }

    fn now(&self) -> u32 {
        ((self.clock)() as i64).wrapping_add(self.offset) as u32
    }

    fn latch_alarm(&mut self) {
        if self.armed && self.now() >= self.alarm {
            self.armed = false;
            self.raw |= 1;
            tracing::debug!(target: "ternvale::rtc", alarm = self.alarm, "rtc match latched");
        }
    }

    fn register(&mut self, reg: u64) -> Option<u32> {
        self.latch_alarm();
        Some(match reg {
            DR => self.now(),
            MR => self.alarm,
            LR => self.load,
            CR => 1,
            IMSC => self.mask,
            RIS => self.raw,
            MIS => self.raw & self.mask,
            ID_BASE..=0xffc => IDS[((reg - ID_BASE) / 4) as usize],
            _ => return None,
        })
    }
}

impl MmioDevice for Pl031 {
    fn name(&self) -> &str {
        "pl031"
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        let reg = offset & !3;
        let Some(word) = self.register(reg) else {
            tracing::warn!(target: "ternvale::rtc", offset = format!("{offset:#x}"), size, "read of unknown pl031 register");
            return 0;
        };
        let lane = (offset & 3) * 8;
        let mask = if size >= 4 {
            u64::from(u32::MAX)
        } else {
            (1u64 << (u32::from(size) * 8)) - 1
        };
        let value = (u64::from(word) >> lane) & mask;
        tracing::trace!(target: "ternvale::rtc", offset = format!("{offset:#x}"), size, value = format!("{value:#x}"), "rtc read");
        value
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        let value = val as u32;
        tracing::trace!(target: "ternvale::rtc", offset = format!("{offset:#x}"), size, value = format!("{value:#x}"), "rtc write");
        match offset {
            LR => {
                self.load = value;
                self.offset = i64::from(value) - (self.clock)() as i64;
                tracing::debug!(target: "ternvale::rtc", seconds = value, offset = self.offset, "rtc time set");
            }
            MR => {
                self.alarm = value;
                self.armed = true;
                tracing::debug!(target: "ternvale::rtc", alarm = value, "rtc match armed");
            }
            CR => {
                tracing::debug!(target: "ternvale::rtc", value, "rtc control write ignored (always enabled)")
            }
            IMSC => self.mask = value & 1,
            ICR => self.raw &= !(value & 1),
            _ => tracing::warn!(
                target: "ternvale::rtc",
                offset = format!("{offset:#x}"),
                size,
                value = format!("{value:#x}"),
                "write to read-only or unknown pl031 register"
            ),
        }
        self.latch_alarm();
    }
}

fn host_seconds() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_secs(),
        Err(error) => {
            tracing::warn!(target: "ternvale::rtc", error = %error, "host clock is before 1970; rtc reads 0");
            0
        }
    }
}

#[cfg(test)]
#[path = "rtc_tests.rs"]
mod tests;
