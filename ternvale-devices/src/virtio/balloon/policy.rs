//! Balloon target: host pressure, idle vCPUs, ram_min, guest in-use.

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_hv::MemoryPressure;
use ternvale_metrics::Metrics;

use super::guest_stats::GuestMem;
use super::pages::GUEST_PAGE;

/// How much of wall time a vCPU may spend in `hv_vcpu_run` and still count idle.
const IDLE_BUSY_FRAC: f64 = 0.05;
const IDLE_WINDOW: Duration = Duration::from_secs(2);

/// Shared policy state.
pub struct Policy {
    pub ram_pages: u32,
    pub floor_pages: u32,
    pub target: AtomicU32,
    pub pressure: AtomicU8,
    pub idle: AtomicU8,
    manual: AtomicU8,
    manual_pages: AtomicU32,
    metrics: Option<Arc<Metrics>>,
    vcpus: u32,
}

impl Policy {
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn new(ram_mib: u64, ram_min_mib: u64, metrics: Option<Arc<Metrics>>, vcpus: u32) -> Self {
        let ram_pages = mib_to_pages(ram_mib);
        let floor_pages = mib_to_pages(ram_min_mib.min(ram_mib));
        Self {
            ram_pages,
            floor_pages,
            target: AtomicU32::new(0),
            pressure: AtomicU8::new(MemoryPressure::Normal as u8),
            idle: AtomicU8::new(1),
            manual: AtomicU8::new(0),
            manual_pages: AtomicU32::new(0),
            metrics,
            vcpus: vcpus.max(1),
        }
    }

    /// Hold `num_pages` until [`Self::clear_manual`]. Capped by floor and guest in-use.
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn set_manual_mib(&self, mib: u64, guest: &GuestMem) -> u32 {
        self.manual.store(1, Ordering::Release);
        let want = mib_to_pages(mib);
        let cap = self.cap_for_guest(guest);
        let target = want.min(cap);
        self.manual_pages.store(target, Ordering::Release);
        tracing::info!(
            target: "ternvale::balloon",
            target_mib = mib,
            pages = target,
            cap,
            "manual balloon target"
        );
        target
    }

    /// Return to host-pressure auto inflate.
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn clear_manual(&self) {
        self.manual.store(0, Ordering::Release);
        tracing::info!(target: "ternvale::balloon", "balloon auto policy");
    }

    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn is_manual(&self) -> bool {
        self.manual.load(Ordering::Acquire) != 0
    }

    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn set_pressure(&self, level: MemoryPressure) {
        self.pressure.store(level as u8, Ordering::Release);
        tracing::info!(
            target: "ternvale::balloon",
            pressure = level.as_str(),
            "balloon pressure"
        );
    }

    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn pressure(&self) -> MemoryPressure {
        match self.pressure.load(Ordering::Acquire) {
            2 => MemoryPressure::Warn,
            4 => MemoryPressure::Critical,
            _ => MemoryPressure::Normal,
        }
    }

    /// Recompute `num_pages`. Never balloons below the floor or guest in-use.
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn refresh(&self, guest: &GuestMem, sample: &mut IdleSample) -> u32 {
        if let Some(metrics) = &self.metrics {
            sample.update(metrics, self.vcpus);
            self.idle.store(u8::from(sample.idle), Ordering::Release);
        }
        let cap = self.cap_for_guest(guest);
        let want = if self.is_manual() {
            self.manual_pages.load(Ordering::Acquire)
        } else if self.pressure() == MemoryPressure::Normal || !sample.idle {
            0
        } else {
            self.ram_pages.saturating_sub(self.floor_pages)
        };
        let target = want.min(cap);
        self.target.store(target, Ordering::Release);
        tracing::debug!(
            target: "ternvale::balloon",
            target,
            cap,
            idle = sample.idle,
            pressure = self.pressure().as_str(),
            "balloon target"
        );
        target
    }

    fn cap_for_guest(&self, guest: &GuestMem) -> u32 {
        let floor = u64::from(self.floor_pages) * GUEST_PAGE;
        let reserved = match guest.in_use_bytes() {
            Some(used) => floor.max(used),
            None => u64::from(self.ram_pages) * GUEST_PAGE,
        };
        let ram = u64::from(self.ram_pages) * GUEST_PAGE;
        ((ram.saturating_sub(reserved)) / GUEST_PAGE) as u32
    }
}

/// Rolling guest-busy sample from Step 61 `guest_ns`.
pub struct IdleSample {
    pub idle: bool,
    last_guest_ns: u64,
    last_at: Instant,
}

impl IdleSample {
    #[tracing::instrument(level = "debug", target = "ternvale::balloon", skip_all)]
    pub fn new() -> Self {
        Self {
            idle: true,
            last_guest_ns: 0,
            last_at: Instant::now(),
        }
    }

    fn update(&mut self, metrics: &Metrics, vcpus: u32) {
        let now = Instant::now();
        let guest_ns: u64 = metrics.snapshot().vcpus.iter().map(|c| c.guest_ns).sum();
        let dt = now.saturating_duration_since(self.last_at);
        if dt >= IDLE_WINDOW {
            let delta = guest_ns.saturating_sub(self.last_guest_ns);
            let wall = dt.as_nanos() as u64 * u64::from(vcpus.max(1));
            let busy = wall > 0 && (delta as f64) > IDLE_BUSY_FRAC * wall as f64;
            self.idle = !busy;
            self.last_guest_ns = guest_ns;
            self.last_at = now;
        }
    }
}

fn mib_to_pages(mib: u64) -> u32 {
    let pages = (mib << 20) / GUEST_PAGE;
    u32::try_from(pages).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_guest_stats_auto_inflate_is_zero() {
        let policy = Policy::new(256, 128, None, 1);
        policy.set_pressure(MemoryPressure::Critical);
        let guest = GuestMem::default();
        let mut idle = IdleSample::new();
        assert_eq!(policy.refresh(&guest, &mut idle), 0);
    }

    #[test]
    fn critical_idle_respects_in_use_and_floor() {
        let policy = Policy::new(256, 64, None, 1);
        policy.set_pressure(MemoryPressure::Warn);
        let guest = GuestMem::default();
        guest.update(super::super::guest_stats::S_MEMTOT, 256 << 20);
        guest.update(super::super::guest_stats::S_AVAIL, 200 << 20);
        let mut idle = IdleSample::new();
        idle.idle = true;
        let target = policy.refresh(&guest, &mut idle);
        let in_use_pages = ((256 - 200) << 20) / GUEST_PAGE;
        let floor_pages = (64 << 20) / GUEST_PAGE;
        let cap = (256 << 20) / GUEST_PAGE - floor_pages.max(in_use_pages);
        assert_eq!(u64::from(target), cap);
    }

    #[test]
    fn manual_target_is_capped_and_survives_normal_pressure() {
        let policy = Policy::new(256, 64, None, 1);
        let guest = GuestMem::default();
        guest.update(super::super::guest_stats::S_MEMTOT, 256 << 20);
        guest.update(super::super::guest_stats::S_AVAIL, 200 << 20);
        policy.set_pressure(MemoryPressure::Normal);
        let pages = policy.set_manual_mib(256, &guest);
        let mut idle = IdleSample::new();
        idle.idle = true;
        assert_eq!(policy.refresh(&guest, &mut idle), pages);
        assert!(policy.is_manual());
        policy.clear_manual();
        assert_eq!(policy.refresh(&guest, &mut idle), 0);
    }
}
