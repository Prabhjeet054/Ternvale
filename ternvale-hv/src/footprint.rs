//! Process `phys_footprint` via `task_info` (TASK_VM_INFO).
//!
//! Raw Mach FFI stays in this crate.

use std::io;
use std::mem::size_of;

/// `TASK_VM_INFO` — Mach task information flavor.
const TASK_VM_INFO: i32 = 22;
const KERN_SUCCESS: i32 = 0;

/// Prefix of `struct task_vm_info` through `phys_footprint`.
/// Layout: XNU `osfmk/mach/task_info.h` `task_vm_info`.
#[repr(C)]
struct TaskVmInfoHead {
    virtual_size: u64,
    region_count: i32,
    page_size: i32,
    resident_size: u64,
    resident_size_peak: u64,
    device: u64,
    device_peak: u64,
    internal: u64,
    internal_peak: u64,
    external: u64,
    external_peak: u64,
    reusable: u64,
    reusable_peak: u64,
    purgeable_volatile_pmap: u64,
    purgeable_volatile_resident: u64,
    purgeable_volatile_virtual: u64,
    compressed: u64,
    compressed_peak: u64,
    compressed_lifetime: u64,
    phys_footprint: u64,
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    fn mach_task_self() -> u32;
    fn task_info(
        target_task: u32,
        flavor: i32,
        task_info_out: *mut i32,
        task_info_count: *mut u32,
    ) -> i32;
}

/// Current process `phys_footprint` in bytes (`footprint` / Activity Monitor).
#[tracing::instrument(level = "debug", target = "ternvale::hv")]
pub fn phys_footprint() -> io::Result<u64> {
    let mut info = TaskVmInfoHead {
        virtual_size: 0,
        region_count: 0,
        page_size: 0,
        resident_size: 0,
        resident_size_peak: 0,
        device: 0,
        device_peak: 0,
        internal: 0,
        internal_peak: 0,
        external: 0,
        external_peak: 0,
        reusable: 0,
        reusable_peak: 0,
        purgeable_volatile_pmap: 0,
        purgeable_volatile_resident: 0,
        purgeable_volatile_virtual: 0,
        compressed: 0,
        compressed_peak: 0,
        compressed_lifetime: 0,
        phys_footprint: 0,
    };
    let mut count = (size_of::<TaskVmInfoHead>() / size_of::<u32>()) as u32;
    // SAFETY: `info` is a prefix of `task_vm_info`; count matches that size.
    let kr = unsafe {
        task_info(
            mach_task_self(),
            TASK_VM_INFO,
            (&raw mut info).cast(),
            &raw mut count,
        )
    };
    if kr != KERN_SUCCESS {
        let error = io::Error::from_raw_os_error(kr);
        tracing::warn!(
            target: "ternvale::hv",
            kr,
            error = %error,
            "task_info(TASK_VM_INFO) failed"
        );
        return Err(error);
    }
    tracing::debug!(
        target: "ternvale::hv",
        bytes = info.phys_footprint,
        "phys_footprint"
    );
    Ok(info.phys_footprint)
}

#[cfg(test)]
mod tests {
    #[test]
    fn phys_footprint_is_nonzero() {
        let bytes = super::phys_footprint().expect("task_info");
        assert!(bytes > 0, "phys_footprint {bytes}");
    }
}
