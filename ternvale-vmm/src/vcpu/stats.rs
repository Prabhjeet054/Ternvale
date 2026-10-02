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

/// Read-only view of one vCPU's counters that outlives the vCPU, for
/// `query-stats` from a thread that does not own it.
#[derive(Clone)]
pub struct VcpuStatsSource(pub(super) std::sync::Arc<Counters>);

impl std::fmt::Debug for VcpuStatsSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("VcpuStatsSource")
            .field(&self.0.snapshot())
            .finish()
    }
}

impl VcpuStatsSource {
    /// Current counter values.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all)]
    pub fn snapshot(&self) -> VcpuStats {
        self.0.snapshot()
    }

    /// A source with no vCPU behind it, for tests.
    #[cfg(test)]
    pub(crate) fn detached() -> Self {
        Self(std::sync::Arc::new(Counters::default()))
    }
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
    cpu_clock_ms(libc::CLOCK_THREAD_CPUTIME_ID, "thread")
}

/// CPU time the whole process (every vCPU and host thread) has used, in
/// milliseconds. Sampled twice around a guest idle period, the difference
/// should be a small fraction of the wall time.
#[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all)]
pub fn process_cpu_ms() -> Option<u64> {
    cpu_clock_ms(libc::CLOCK_PROCESS_CPUTIME_ID, "process")
}

fn cpu_clock_ms(clock: libc::clockid_t, which: &'static str) -> Option<u64> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable timespec. The clock is one of the
    // CPU-time clocks and clock_gettime writes only `now`.
    let rc = unsafe { libc::clock_gettime(clock, &mut now) };
    if rc != 0 {
        tracing::warn!(target: "ternvale::vcpu", rc, clock = which, "cpu clock unavailable");
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
    fn process_cpu_time_covers_this_thread() {
        let thread = thread_cpu_ms().expect("thread clock");
        let process = process_cpu_ms().expect("process clock");
        assert!(process >= thread, "process={process} thread={thread}");
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
