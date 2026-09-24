//! Hand-written FFI for the arm64 Hypervisor.framework VM calls we use.
//!
//! Signatures match `hv_vm.h` in the macOS 27 SDK (`hv_vm_create` takes a
//! nullable `hv_vm_config_t`; `hv_vm_destroy` takes no arguments). Return
//! values are `hv_return_t` (`mach_error_t`, a 32-bit `int`).

use std::ffi::c_void;

#[link(name = "Hypervisor", kind = "framework")]
unsafe extern "C" {
    pub(crate) fn hv_vm_create(config: *mut c_void) -> i32;
    pub(crate) fn hv_vm_destroy() -> i32;
    /// `hv_vm.h`: page-aligned `addr` and `ipa`, `size` a multiple of the page size.
    pub(crate) fn hv_vm_map(addr: *mut c_void, ipa: u64, size: usize, flags: u64) -> i32;
    pub(crate) fn hv_vm_unmap(ipa: u64, size: usize) -> i32;
    /// `hv_vcpu.h`: one vCPU per thread. `config` NULL is the default.
    /// TODO(verify): the header marks `config` nullable but does not say NULL is the default.
    pub(crate) fn hv_vcpu_create(
        vcpu: *mut u64,
        exit: *mut *mut VcpuExit,
        config: *mut c_void,
    ) -> i32;
    pub(crate) fn hv_vcpu_destroy(vcpu: u64) -> i32;
    pub(crate) fn hv_vcpu_get_reg(vcpu: u64, reg: u32, value: *mut u64) -> i32;
    pub(crate) fn hv_vcpu_set_reg(vcpu: u64, reg: u32, value: u64) -> i32;
    pub(crate) fn hv_vcpu_get_sys_reg(vcpu: u64, reg: u16, value: *mut u64) -> i32;
    pub(crate) fn hv_vcpu_set_sys_reg(vcpu: u64, reg: u16, value: u64) -> i32;
    pub(crate) fn hv_vcpu_run(vcpu: u64) -> i32;
    pub(crate) fn hv_vcpus_exit(vcpus: *const u64, vcpu_count: u32) -> i32;
    /// `hv_vcpu.h`: `true` masks VTimer exits. The VTIMER_ACTIVATED exit already sets the mask.
    pub(crate) fn hv_vcpu_set_vtimer_mask(vcpu: u64, masked: bool) -> i32;
}

// GICv3 symbols are resolved with `dlsym` so a host without them returns
// `HvError::GicUnavailable` instead of failing at load. `hv_gic.h` marks each
// call `API_AVAILABLE(macos(15.0))`.
#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    pub(crate) fn dlsym(handle: *mut c_void, symbol: *const i8) -> *mut c_void;
    pub(crate) fn os_release(object: *mut c_void);
    pub(crate) fn sysctlbyname(
        name: *const i8,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *mut c_void,
        newlen: usize,
    ) -> i32;
}

/// `hv_vcpu_exit_t` from `hv_vcpu_types.h`. `reason` is at 0; `exception` is at 8.
#[repr(C)]
pub struct VcpuExit {
    pub reason: u32,
    _pad: u32,
    pub syndrome: u64,
    pub virtual_address: u64,
    pub physical_address: u64,
}
