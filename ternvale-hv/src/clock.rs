//! Host counter for vtimer offsets.
//!
//! Not part of Hypervisor.framework, but raw FFI is kept in this crate.

use crate::ffi::mach_absolute_time;

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

#[cfg(test)]
mod tests {
    use super::host_ticks;

    #[test]
    fn host_ticks_advance() {
        let first = host_ticks();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(host_ticks() > first);
    }
}
