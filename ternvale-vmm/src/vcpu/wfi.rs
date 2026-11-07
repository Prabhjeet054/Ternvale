//! How long a WFI/WFE should park: until the virtual timer fires or a host
//! interrupt arrives. The vCPU thread is not throttled; it only sleeps while
//! the guest itself asked to wait.

use std::time::Duration;

/// ARM ARM DDI 0487: `CNTV_CTL_EL0.ENABLE`.
pub(super) const CNTV_CTL_ENABLE: u64 = 1;
/// ARM ARM DDI 0487: `CNTV_CTL_EL0.IMASK`.
pub(super) const CNTV_CTL_IMASK: u64 = 1 << 1;

/// Park policy for one WFI/WFE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Park {
    /// `CVAL` is already due: return to `hv_vcpu_run` so a VTIMER exit / PPI
    /// can be taken. Do not sleep.
    Ready,
    /// Timer disabled or masked: wait on the WFI condvar until
    /// [`crate::vcpu::VcpuStop::nudge`] or stop.
    UntilInterrupt,
    /// Sleep at most this long, or until a nudge.
    Until(Duration),
}

/// Decide the park from `CNTV_CTL_EL0`, `CNTV_CVAL_EL0`, and `CNTVCT_EL0`
/// (`mach_absolute_time() - vtimer_offset`, per `hv_vcpu.h`).
///
/// `ticks_to_duration` converts remaining counter ticks using the host
/// timebase. Callers pass [`ternvale_hv::ticks_to_duration`].
#[tracing::instrument(
    level = "debug",
    target = "ternvale::vcpu",
    skip_all,
    fields(
        ctl = %format!("{ctl:#x}"),
        cval = %format!("{cval:#x}"),
        cntvct = %format!("{cntvct:#x}")
    )
)]
pub(super) fn park_until(
    ctl: u64,
    cval: u64,
    cntvct: u64,
    ticks_to_duration: impl Fn(u64) -> Duration,
) -> Park {
    if ctl & CNTV_CTL_ENABLE == 0 || ctl & CNTV_CTL_IMASK != 0 {
        tracing::debug!(
            target: "ternvale::vcpu",
            enable = ctl & CNTV_CTL_ENABLE != 0,
            imask = ctl & CNTV_CTL_IMASK != 0,
            "wfi waits for interrupt; vtimer will not fire"
        );
        return Park::UntilInterrupt;
    }
    if cval <= cntvct {
        tracing::debug!(
            target: "ternvale::vcpu",
            "wfi: vtimer already due, not parking"
        );
        return Park::Ready;
    }
    let wait = ticks_to_duration(cval - cntvct);
    tracing::debug!(
        target: "ternvale::vcpu",
        wait_ns = wait.as_nanos() as u64,
        "wfi park until vtimer CVAL"
    );
    Park::Until(wait)
}

#[cfg(test)]
mod tests {
    use super::{park_until, Park, CNTV_CTL_ENABLE, CNTV_CTL_IMASK};
    use std::time::Duration;

    fn ns(ticks: u64) -> Duration {
        Duration::from_nanos(ticks)
    }

    #[test]
    fn disabled_or_masked_timer_waits_for_interrupt() {
        assert_eq!(park_until(0, 100, 0, ns), Park::UntilInterrupt);
        assert_eq!(
            park_until(CNTV_CTL_ENABLE | CNTV_CTL_IMASK, 100, 0, ns),
            Park::UntilInterrupt
        );
    }

    #[test]
    fn due_timer_does_not_park() {
        assert_eq!(park_until(CNTV_CTL_ENABLE, 10, 10, ns), Park::Ready);
        assert_eq!(park_until(CNTV_CTL_ENABLE, 9, 10, ns), Park::Ready);
    }

    #[test]
    fn future_cval_sleeps_the_remaining_ticks() {
        assert_eq!(
            park_until(CNTV_CTL_ENABLE, 1_000, 250, ns),
            Park::Until(Duration::from_nanos(750))
        );
    }
}
