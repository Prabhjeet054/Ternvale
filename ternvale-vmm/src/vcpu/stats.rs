//! Per-vCPU activity counters, logged when the vCPU is destroyed.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// What one vCPU did since it was created.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VcpuStats {
    /// `hv_vcpu_run` calls.
    pub runs: u64,
    /// Wall time inside `hv_vcpu_run`, in milliseconds. Linux's idle WFI
    /// stays inside the run (it does not trap), so this is close to the
    /// vCPU's powered-on time, not its busy time. The `thread_cpu_ms` field
    /// of the "vCPU destroyed" log is the busy measure.
    pub guest_ms: u64,
    /// WFI exits that parked the thread.
    pub wfi_parks: u64,
    /// Wall time parked on WFI, in milliseconds.
    pub park_ms: u64,
    /// Virtual-timer exits.
    pub vtimer_exits: u64,
}

#[derive(Default)]
pub(super) struct Counters {
    runs: AtomicU64,
    guest_ns: AtomicU64,
    wfi_parks: AtomicU64,
    park_ns: AtomicU64,
    vtimer_exits: AtomicU64,
}

/// CPU time the calling thread has used, in milliseconds. On a vCPU thread
/// this is the host cost of the guest's work; an idle WFI inside
/// `hv_vcpu_run` adds little.
pub(super) fn thread_cpu_ms() -> Option<u64> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable timespec. CLOCK_THREAD_CPUTIME_ID
    // reads the calling thread's CPU clock and writes only `now`.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut now) };
    if rc != 0 {
        tracing::warn!(target: "ternvale::vcpu", rc, "thread cpu clock unavailable");
        return None;
    }
    let secs = u64::try_from(now.tv_sec).ok()?;
    let nanos = u64::try_from(now.tv_nsec).ok()?;
    Some(secs * 1_000 + nanos / 1_000_000)
}

fn nanos(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
}

impl Counters {
    pub(super) fn run(&self, elapsed: Duration) {
        self.runs.fetch_add(1, Ordering::Relaxed);
        self.guest_ns.fetch_add(nanos(elapsed), Ordering::Relaxed);
    }

    pub(super) fn park(&self, elapsed: Duration) {
        self.wfi_parks.fetch_add(1, Ordering::Relaxed);
        self.park_ns.fetch_add(nanos(elapsed), Ordering::Relaxed);
    }

    pub(super) fn vtimer(&self) {
        self.vtimer_exits.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn snapshot(&self) -> VcpuStats {
        VcpuStats {
            runs: self.runs.load(Ordering::Relaxed),
            guest_ms: self.guest_ns.load(Ordering::Relaxed) / 1_000_000,
            wfi_parks: self.wfi_parks.load(Ordering::Relaxed),
            park_ms: self.park_ns.load(Ordering::Relaxed) / 1_000_000,
            vtimer_exits: self.vtimer_exits.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_cpu_time_grows_with_work() {
        let before = thread_cpu_ms().expect("cpu clock");
        let started = std::time::Instant::now();
        let mut spin = 0u64;
        while started.elapsed() < Duration::from_millis(30) {
            spin = std::hint::black_box(spin.wrapping_add(1));
        }
        let after = thread_cpu_ms().expect("cpu clock");
        assert!(after >= before + 20, "before={before} after={after}");
    }

    #[test]
    fn counters_accumulate_into_milliseconds() {
        let counters = Counters::default();
        counters.run(Duration::from_micros(1_500));
        counters.run(Duration::from_micros(1_500));
        counters.park(Duration::from_millis(10));
        counters.vtimer();
        assert_eq!(
            counters.snapshot(),
            VcpuStats {
                runs: 2,
                guest_ms: 3,
                wfi_parks: 1,
                park_ms: 10,
                vtimer_exits: 1,
            }
        );
    }
}
