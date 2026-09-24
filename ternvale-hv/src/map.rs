//! `hv_vm_map` and `hv_vm_unmap` for the process-wide VM.
//!
//! Flags match `usr/include/arm64/hv/hv_kern_types.h`: `HV_MEMORY_READ` is
//! `1 << 0`, `HV_MEMORY_WRITE` is `1 << 1`, and `HV_MEMORY_EXEC` is `1 << 2`.
//! `hv_vm.h` requires a page-aligned host address, IPA, and size. It does not
//! say the call must run on the thread that created the VM.
//!
//! TODO(verify): whether `hv_vm_unmap` must run on the same thread as `hv_vm_map`.

use std::ptr::NonNull;

use crate::error::{HvError, HV_SUCCESS};
use crate::ffi::{hv_vm_map, hv_vm_unmap};

/// `HV_MEMORY_READ`.
pub const HV_MEMORY_READ: u64 = 1 << 0;
/// `HV_MEMORY_WRITE`.
pub const HV_MEMORY_WRITE: u64 = 1 << 1;
/// `HV_MEMORY_EXEC`.
pub const HV_MEMORY_EXEC: u64 = 1 << 2;
/// Read, write, and execute. Guest RAM uses this set.
pub const HV_MEMORY_RWX: u64 = HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC;

/// Map `size` host bytes at `host` into the current VM at guest IPA `ipa`.
///
/// `host` must stay allocated until [`unmap_memory`] for the same `ipa` and
/// `size`. A VM must already exist. This function does not take ownership of
/// the buffer.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::hv",
    skip(host),
    fields(ipa = format!("{:#x}", ipa), size = format!("{:#x}", size), flags = format!("{:#x}", flags))
)]
pub fn map_memory(host: NonNull<u8>, ipa: u64, size: usize, flags: u64) -> Result<(), HvError> {
    let args = format!(
        "addr={:#x} ipa={:#x} size={:#x} flags={:#x}",
        host.as_ptr() as usize,
        ipa,
        size,
        flags
    );
    // SAFETY: `host` is non-null. The caller guarantees it points at `size`
    // readable and writable bytes that remain allocated until unmap, and that
    // `hv_vm_create` has succeeded. `hv_vm_map` does not free the buffer.
    let raw = unsafe { hv_vm_map(host.as_ptr().cast(), ipa, size, flags) };
    let code = ternvale_log::log_hv_call!("hv_vm_map", args, raw);
    if code != HV_SUCCESS {
        let error = HvError::from_code(code);
        tracing::error!(
            target: "ternvale::hv",
            code = format!("{:#x}", code),
            error = %error,
            "hv_vm_map failed"
        );
        return Err(error);
    }
    Ok(())
}

/// Unmap `size` bytes at guest IPA `ipa` from the current VM.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::hv",
    skip_all,
    fields(ipa = format!("{:#x}", ipa), size = format!("{:#x}", size))
)]
pub fn unmap_memory(ipa: u64, size: usize) -> Result<(), HvError> {
    let args = format!("ipa={ipa:#x} size={size:#x}");
    // SAFETY: `ipa` and `size` describe a region previously passed to
    // `hv_vm_map` for the current VM. The call takes no host pointer.
    let raw = unsafe { hv_vm_unmap(ipa, size) };
    let code = ternvale_log::log_hv_call!("hv_vm_unmap", args, raw);
    if code != HV_SUCCESS {
        let error = HvError::from_code(code);
        tracing::error!(
            target: "ternvale::hv",
            code = format!("{:#x}", code),
            error = %error,
            "hv_vm_unmap failed"
        );
        return Err(error);
    }
    Ok(())
}
