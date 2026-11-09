//! Hand-written FFI for the arm64 Hypervisor.framework VM calls we use.
//!
//! Signatures match `hv_vm.h` in the macOS 27 SDK (`hv_vm_create` takes a
//! nullable `hv_vm_config_t`; `hv_vm_destroy` takes no arguments). Return
//! values are `hv_return_t` (`mach_error_t`, a 32-bit `int`).

use std::ffi::{c_char, c_void};

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
    /// `hv_vcpu.h`: `CNTVCT_EL0 = mach_absolute_time() - vtimer_offset`.
    pub(crate) fn hv_vcpu_get_vtimer_offset(vcpu: u64, offset: *mut u64) -> i32;
    pub(crate) fn hv_vcpu_set_vtimer_offset(vcpu: u64, offset: u64) -> i32;
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
    /// `<sys/random.h>`: at most 256 bytes per call; returns 0 or -1 with `errno`.
    pub(crate) fn getentropy(buf: *mut c_void, buflen: usize) -> i32;
    /// `<mach/mach_time.h>`: the host counter the vtimer offset is relative to.
    pub(crate) fn mach_absolute_time() -> u64;
    /// `<mach/mach_time.h>`: convert `mach_absolute_time` ticks to nanoseconds
    /// (`ticks * numer / denom`).
    pub(crate) fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
    pub(crate) fn getsockopt(
        socket: i32,
        level: i32,
        name: i32,
        value: *mut c_void,
        len: *mut u32,
    ) -> i32;
    pub(crate) fn recv(socket: i32, buffer: *mut c_void, length: usize, flags: i32) -> isize;
}

/// Darwin `MSG_PEEK`.
pub(crate) const MSG_PEEK: i32 = 0x02;

#[repr(C)]
pub(crate) struct CfDictionaryKeyCallBacks {
    pub version: isize,
    pub retain: *const c_void,
    pub release: *const c_void,
    pub copy_description: *const c_void,
    pub equal: *const c_void,
    pub hash: *const c_void,
}

#[repr(C)]
pub(crate) struct CfDictionaryValueCallBacks {
    pub version: isize,
    pub retain: *const c_void,
    pub release: *const c_void,
    pub copy_description: *const c_void,
    pub equal: *const c_void,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub(crate) static kCFTypeDictionaryKeyCallBacks: CfDictionaryKeyCallBacks;
    pub(crate) static kCFTypeDictionaryValueCallBacks: CfDictionaryValueCallBacks;
    pub(crate) fn CFRelease(cf: *const c_void);
    pub(crate) fn CFStringCreateWithCString(
        alloc: *mut c_void,
        c_str: *const c_char,
        encoding: u32,
    ) -> *mut c_void;
    pub(crate) fn CFStringGetCStringPtr(the_string: *mut c_void, encoding: u32) -> *const c_char;
    pub(crate) fn CFStringGetCString(
        the_string: *mut c_void,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
    pub(crate) fn CFNumberCreate(
        alloc: *mut c_void,
        number_type: isize,
        value: *const c_void,
    ) -> *mut c_void;
    pub(crate) fn CFDictionaryCreateMutable(
        alloc: *mut c_void,
        capacity: isize,
        key_callbacks: *const CfDictionaryKeyCallBacks,
        value_callbacks: *const CfDictionaryValueCallBacks,
    ) -> *mut c_void;
    pub(crate) fn CFDictionarySetValue(
        the_dict: *mut c_void,
        key: *const c_void,
        value: *const c_void,
    );
    pub(crate) fn CFDataCreate(alloc: *mut c_void, bytes: *const u8, length: isize) -> *mut c_void;
    pub(crate) fn CFDataGetLength(the_data: *mut c_void) -> isize;
    pub(crate) fn CFDataGetBytePtr(the_data: *mut c_void) -> *const u8;
    pub(crate) fn CFURLCreateWithFileSystemPath(
        alloc: *mut c_void,
        path: *mut c_void,
        path_style: usize,
        is_directory: u8,
    ) -> *mut c_void;
    pub(crate) fn CFURLCreateBookmarkData(
        alloc: *mut c_void,
        url: *mut c_void,
        options: usize,
        resource_properties: *mut c_void,
        relative_to: *mut c_void,
        error: *mut *mut c_void,
    ) -> *mut c_void;
    pub(crate) fn CFURLCreateByResolvingBookmarkData(
        alloc: *mut c_void,
        bookmark: *mut c_void,
        options: usize,
        relative_to: *mut c_void,
        resource_properties: *mut c_void,
        is_stale: *mut u8,
        error: *mut *mut c_void,
    ) -> *mut c_void;
    pub(crate) fn CFURLStartAccessingSecurityScopedResource(url: *mut c_void) -> u8;
    pub(crate) fn CFURLStopAccessingSecurityScopedResource(url: *mut c_void);
    pub(crate) fn CFURLGetFileSystemRepresentation(
        url: *mut c_void,
        resolve_against_base: u8,
        buffer: *mut u8,
        max_buf_len: isize,
    ) -> u8;
    pub(crate) fn CFErrorCopyDescription(err: *mut c_void) -> *mut c_void;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    pub(crate) fn SecRequirementCreateWithString(
        text: *mut c_void,
        flags: u32,
        requirement: *mut *mut c_void,
    ) -> i32;
    pub(crate) fn SecCodeCopyGuestWithAttributes(
        host: *mut c_void,
        attributes: *mut c_void,
        flags: u32,
        guest: *mut *mut c_void,
    ) -> i32;
    pub(crate) fn SecCodeCheckValidity(
        code: *mut c_void,
        flags: u32,
        requirement: *mut c_void,
    ) -> i32;
}

pub(crate) unsafe fn cf_release(obj: *mut c_void) {
    // SAFETY: caller owns a +1 CFType reference.
    unsafe { CFRelease(obj as *const c_void) };
}

/// `mach_timebase_info_data_t`.
#[repr(C)]
pub(crate) struct MachTimebaseInfo {
    pub numer: u32,
    pub denom: u32,
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
