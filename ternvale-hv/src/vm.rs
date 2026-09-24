//! One Hypervisor.framework VM per process.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{HvError, HV_SUCCESS};
use crate::ffi::{hv_vm_create, hv_vm_destroy};

/// Set while a [`Vm`] is alive. Cleared after a successful `hv_vm_destroy`.
static VM_EXISTS: AtomicBool = AtomicBool::new(false);

/// The process-wide Hypervisor.framework VM.
///
/// `hv_vm_create(NULL)` uses the default config (`hv_vm.h`). The config pointer
/// stays inside this crate.
///
/// TODO(verify): Apple's headers do not say whether `hv_vm_destroy` must run
/// on the same thread as `hv_vm_create`. `Vm` is thread-affine until that is
/// confirmed.
pub struct Vm {
    _thread_affine: PhantomData<*const ()>,
}

impl std::fmt::Debug for Vm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Vm")
    }
}

impl Vm {
    /// Create the only VM in this process.
    #[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all)]
    pub fn create() -> Result<Self, HvError> {
        if VM_EXISTS.swap(true, Ordering::AcqRel) {
            tracing::error!(
                target: "ternvale::hv",
                "rejected hv_vm_create; a VM already exists in this process"
            );
            return Err(HvError::AlreadyExists);
        }
        tracing::debug!(target: "ternvale::hv", "creating VM with default config");
        // SAFETY: `config` is NULL, which hv_vm.h documents as the default
        // configuration. The atomic flag guarantees no other Vm is alive, so
        // this is the process's single hv_vm_create.
        let raw = unsafe { hv_vm_create(std::ptr::null_mut()) };
        let code = ternvale_log::log_hv_call!("hv_vm_create", "config=null", raw);
        if code != HV_SUCCESS {
            VM_EXISTS.store(false, Ordering::Release);
            let error = HvError::from_code(code);
            tracing::error!(
                target: "ternvale::hv",
                code = format!("{:#x}", code),
                error = %error,
                "hv_vm_create failed"
            );
            return Err(error);
        }
        tracing::info!(target: "ternvale::hv", "VM created");
        Ok(Self {
            _thread_affine: PhantomData,
        })
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        // SAFETY: this value is the only Vm, Drop runs once, and no public
        // pointer aliases the kernel VM. hv_vm_destroy takes no arguments.
        let raw = unsafe { hv_vm_destroy() };
        let code = ternvale_log::log_hv_call!("hv_vm_destroy", "none", raw);
        if code == HV_SUCCESS {
            VM_EXISTS.store(false, Ordering::Release);
            tracing::info!(target: "ternvale::hv", "VM destroyed");
        } else {
            // Leave VM_EXISTS set so a later create cannot stack a second VM
            // on a destroy that the kernel rejected.
            let error = HvError::from_code(code);
            tracing::error!(
                target: "ternvale::hv",
                code = format!("{:#x}", code),
                error = %error,
                "hv_vm_destroy failed; VM slot remains taken"
            );
        }
    }
}
