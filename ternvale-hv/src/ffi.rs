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
}
