//! ARM SBSA generic watchdog (two 4 KiB frames).
//!
//! Linux `drivers/watchdog/sbsa_gwdt.c`, compatible `arm,sbsa-gwdt`.
//! Register map: ARM Server Base System Architecture (SBSA) Generic Watchdog
//! — refresh frame `WRR` at 0x000; control frame `WCS` 0x000, `WOR` 0x008,
//! `WCV` 0x010, `W_IIDR` 0xFCC. `WCS` bits: EN=0, WS0=1, WS1=2.
//! TODO(verify): compare WOR/WCV against CNTVCT at CNTFRQ (QEMU virt is 24 MHz);
//! this model treats WOR as a millisecond timeout for host tests.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::crash::{CrashHub, CrashKind};
use crate::mmio::MmioDevice;

/// Refresh / control frame size the Linux driver maps (`SBSA_GWDT_WRR` page).
pub const SBSA_FRAME: u64 = 0x1000;
/// `WRR` (refresh).
const WRR: u64 = 0x000;
/// `WCS`.
const WCS: u64 = 0x000;
/// `WOR`.
const WOR: u64 = 0x008;
/// `WCV` low.
const WCV: u64 = 0x010;
/// `WCV` high (64-bit compare value).
const WCV_HI: u64 = 0x014;
/// `W_IIDR`.
const IIDR: u64 = 0xFCC;
/// `WCS.EN`.
pub const WCS_EN: u32 = 1 << 0;
/// `WCS.WS0`.
pub const WCS_WS0: u32 = 1 << 1;
/// `WCS.WS1`.
pub const WCS_WS1: u32 = 1 << 2;
/// ARM implementer in `W_IIDR` (bits 19:0 jep106).
const IIDR_VALUE: u32 = 0x1043B;

struct Inner {
    wcs: AtomicU32,
    wor: AtomicU32,
    wcv: AtomicU64,
    deadline: MutexDeadline,
    hub: Arc<CrashHub>,
    irq: Arc<dyn Fn(bool) + Send + Sync>,
    irq_level: AtomicBool,
}

/// Instant deadline behind a mutex (not on the MMIO hot path unless armed).
struct MutexDeadline {
    inner: std::sync::Mutex<Option<Instant>>,
}

impl MutexDeadline {
    fn set(&self, at: Option<Instant>) {
        *crate::lockwatch::lock(&self.inner, "sbsa-deadline") = at;
    }

    fn get(&self) -> Option<Instant> {
        *crate::lockwatch::lock(&self.inner, "sbsa-deadline")
    }
}

/// Shared SBSA watchdog state (refresh + control frames).
#[derive(Clone)]
pub struct SbsaWatchdog {
    inner: Arc<Inner>,
    control: bool,
}

impl SbsaWatchdog {
    /// Pair of MMIO devices sharing one watchdog. `irq` is WS0 (SPI).
    #[tracing::instrument(level = "debug", target = "ternvale::watchdog", skip_all)]
    pub fn pair(hub: Arc<CrashHub>, irq: Arc<dyn Fn(bool) + Send + Sync>) -> (Self, Self) {
        let inner = Arc::new(Inner {
            wcs: AtomicU32::new(0),
            wor: AtomicU32::new(0),
            wcv: AtomicU64::new(0),
            deadline: MutexDeadline {
                inner: std::sync::Mutex::new(None),
            },
            hub,
            irq,
            irq_level: AtomicBool::new(false),
        });
        (
            Self {
                inner: Arc::clone(&inner),
                control: false,
            },
            Self {
                inner,
                control: true,
            },
        )
    }

    /// Advance time: if enabled and the deadline passed, raise WS0 then WS1.
    #[tracing::instrument(level = "debug", target = "ternvale::watchdog", skip_all)]
    pub fn poll(&self, now: Instant) {
        expire(&self.inner, now);
    }

    /// Test helper: fire as if WOR elapsed.
    #[cfg(test)]
    pub fn force_expire(&self) {
        let now = Instant::now();
        self.inner.deadline.set(Some(now));
        expire(&self.inner, now + Duration::from_millis(1));
    }
}

fn kick(inner: &Inner) {
    let wor = inner.wor.load(Ordering::Acquire);
    if inner.wcs.load(Ordering::Acquire) & WCS_EN == 0 {
        inner.deadline.set(None);
        return;
    }
    let wait = Duration::from_millis(u64::from(wor.max(1)));
    inner.deadline.set(Some(Instant::now() + wait));
    inner.wcs.fetch_and(!(WCS_WS0 | WCS_WS1), Ordering::AcqRel);
    set_irq(inner, false);
    tracing::debug!(target: "ternvale::watchdog", wor, "SBSA watchdog refreshed");
}

fn expire(inner: &Inner, now: Instant) {
    if inner.wcs.load(Ordering::Acquire) & WCS_EN == 0 {
        return;
    }
    let Some(deadline) = inner.deadline.get() else {
        return;
    };
    if now < deadline {
        return;
    }
    let prev = inner.wcs.load(Ordering::Acquire);
    if prev & WCS_WS1 != 0 {
        return;
    }
    if prev & WCS_WS0 == 0 {
        inner.wcs.fetch_or(WCS_WS0, Ordering::AcqRel);
        tracing::error!(
            target: "ternvale::watchdog",
            reason = "refresh timeout",
            "SBSA watchdog WS0"
        );
        set_irq(inner, true);
        // Second window: WS1 after another WOR (Linux sbsa_gwdt pretimeout).
        let wor = inner.wor.load(Ordering::Acquire);
        inner
            .deadline
            .set(Some(now + Duration::from_millis(u64::from(wor.max(1)))));
        return;
    }
    inner.wcs.fetch_or(WCS_WS1, Ordering::AcqRel);
    tracing::error!(
        target: "ternvale::watchdog",
        reason = "WS1 after WS0",
        "SBSA watchdog WS1"
    );
    inner.hub.report(CrashKind::Watchdog);
}

fn set_irq(inner: &Inner, level: bool) {
    if inner.irq_level.swap(level, Ordering::AcqRel) != level {
        (inner.irq)(level);
    }
}

fn read_control(inner: &Inner, offset: u64, size: u8) -> u64 {
    match (offset, size) {
        (WCS, 4 | 1) => u64::from(inner.wcs.load(Ordering::Acquire)),
        (WOR, 4) => u64::from(inner.wor.load(Ordering::Acquire)),
        (WCV, 4) => inner.wcv.load(Ordering::Acquire) as u32 as u64,
        (WCV_HI, 4) => inner.wcv.load(Ordering::Acquire) >> 32,
        (IIDR, 4) => u64::from(IIDR_VALUE),
        _ => {
            tracing::warn!(
                target: "ternvale::watchdog",
                offset = format!("{offset:#x}"),
                size,
                "SBSA control rejected read"
            );
            0
        }
    }
}

fn write_control(inner: &Inner, offset: u64, size: u8, val: u64) {
    match (offset, size) {
        (WCS, 4 | 1) => {
            let bits = val as u32 & WCS_EN;
            inner.wcs.store(bits, Ordering::Release);
            if bits & WCS_EN != 0 {
                tracing::info!(target: "ternvale::watchdog", "SBSA watchdog enabled");
                kick(inner);
            } else {
                inner.deadline.set(None);
                set_irq(inner, false);
                tracing::info!(target: "ternvale::watchdog", "SBSA watchdog disabled");
            }
        }
        (WOR, 4) => {
            inner.wor.store(val as u32, Ordering::Release);
            kick(inner);
        }
        (WCV, 4) => {
            let hi = inner.wcv.load(Ordering::Acquire) & !0xffff_ffff;
            inner.wcv.store(hi | (val as u32 as u64), Ordering::Release);
        }
        (WCV_HI, 4) => {
            let lo = inner.wcv.load(Ordering::Acquire) & 0xffff_ffff;
            inner.wcv.store(lo | (val << 32), Ordering::Release);
        }
        _ => tracing::warn!(
            target: "ternvale::watchdog",
            offset = format!("{offset:#x}"),
            size,
            value = format!("{val:#x}"),
            "SBSA control rejected write"
        ),
    }
}

impl MmioDevice for SbsaWatchdog {
    fn name(&self) -> &str {
        if self.control {
            "sbsa-gwdt-control"
        } else {
            "sbsa-gwdt-refresh"
        }
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        if self.control {
            read_control(&self.inner, offset, size)
        } else {
            0
        }
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        if self.control {
            write_control(&self.inner, offset, size, val);
            return;
        }
        if offset == WRR {
            kick(&self.inner);
            tracing::trace!(
                target: "ternvale::watchdog",
                value = format!("{val:#x}"),
                "SBSA WRR kick"
            );
            return;
        }
        tracing::warn!(
            target: "ternvale::watchdog",
            offset = format!("{offset:#x}"),
            size,
            "SBSA refresh rejected write"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash::CrashHub;

    #[test]
    fn enable_without_refresh_reports_watchdog() {
        let hub = CrashHub::new();
        let irq = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let irq_fn = {
            let irq = Arc::clone(&irq);
            Arc::new(move |level: bool| irq.store(level, Ordering::SeqCst))
                as Arc<dyn Fn(bool) + Send + Sync>
        };
        let (mut refresh, mut ctrl) = SbsaWatchdog::pair(hub, irq_fn);
        ctrl.write(WOR, 4, 1);
        ctrl.write(WCS, 4, u64::from(WCS_EN));
        assert_eq!(ctrl.read(WCS, 4) & u64::from(WCS_EN), u64::from(WCS_EN));
        refresh.force_expire();
        assert_ne!(ctrl.read(WCS, 4) & u64::from(WCS_WS0), 0);
        refresh.force_expire();
        assert_ne!(ctrl.read(WCS, 4) & u64::from(WCS_WS1), 0);
        refresh.write(WRR, 4, 0);
        assert_eq!(ctrl.read(WCS, 4) & u64::from(WCS_WS0 | WCS_WS1), 0);
    }
}
