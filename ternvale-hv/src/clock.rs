//! Host counter for vtimer offsets.
//!
//! Not part of Hypervisor.framework, but raw FFI is kept in this crate.

use std::sync::OnceLock;
use std::time::Duration;

use crate::ffi::{mach_absolute_time, mach_timebase_info, MachTimebaseInfo};

/// `mach_absolute_time()`, the counter `hv_vcpu.h` defines the vtimer against
/// (`CNTVCT_EL0 = mach_absolute_time() - vtimer_offset`). Adding the ticks a
/// VM spent paused to every vCPU's offset hides the pause from the guest.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all)]
pub fn host_ticks() -> u64 {
    // SAFETY: `mach_absolute_time` takes no arguments and only reads the clock.
    let ticks = unsafe { mach_absolute_time() };
    tracing::trace!(target: "ternvale::hv", ticks, "mach_absolute_time");
    ticks
}

/// Convert `mach_absolute_time` ticks to a [`Duration`] using the host
/// `mach_timebase_info` ratio. Apple Silicon is typically 1:1 nanoseconds.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all, fields(ticks))]
pub fn ticks_to_duration(ticks: u64) -> Duration {
    let (numer, denom) = timebase();
    ticks_to_duration_scaled(ticks, numer, denom)
}

/// Same as [`ticks_to_duration`] with an explicit `numer/denom` ratio.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::hv",
    skip_all,
    fields(ticks, numer, denom)
)]
pub fn ticks_to_duration_scaled(ticks: u64, numer: u32, denom: u32) -> Duration {
    if ticks == 0 || numer == 0 || denom == 0 {
        return Duration::ZERO;
    }
    let ns = (u128::from(ticks) * u128::from(numer)) / u128::from(denom);
    Duration::from_nanos(ns.min(u128::from(u64::MAX)) as u64)
}

fn timebase() -> (u32, u32) {
    static TIMEBASE: OnceLock<(u32, u32)> = OnceLock::new();
    *TIMEBASE.get_or_init(|| {
        let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
        // SAFETY: `info` is a valid `mach_timebase_info_data_t`; the call only writes it.
        let rc = unsafe { mach_timebase_info(&mut info) };
        if rc != 0 || info.numer == 0 || info.denom == 0 {
            tracing::error!(
                target: "ternvale::hv",
                rc,
                numer = info.numer,
                denom = info.denom,
                "mach_timebase_info failed; treating ticks as nanoseconds"
            );
            (1, 1)
        } else {
            tracing::info!(
                target: "ternvale::hv",
                numer = info.numer,
                denom = info.denom,
                "mach timebase"
            );
            (info.numer, info.denom)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{host_ticks, ticks_to_duration, ticks_to_duration_scaled};
    use std::time::Duration;

    #[test]
    fn host_ticks_advance() {
        let first = host_ticks();
        std::thread::sleep(Duration::from_millis(2));
        assert!(host_ticks() > first);
    }

    #[test]
    fn ticks_scale_with_timebase() {
        assert_eq!(
            ticks_to_duration_scaled(1_000, 1, 1),
            Duration::from_nanos(1_000)
        );
        assert_eq!(
            ticks_to_duration_scaled(2, 1_000_000_000, 1),
            Duration::from_secs(2)
        );
        assert_eq!(ticks_to_duration_scaled(0, 1, 1), Duration::ZERO);
        assert_eq!(ticks_to_duration_scaled(10, 1, 0), Duration::ZERO);
    }

    #[test]
    fn host_timebase_is_positive() {
        assert!(ticks_to_duration(1) > Duration::ZERO);
    }
}
