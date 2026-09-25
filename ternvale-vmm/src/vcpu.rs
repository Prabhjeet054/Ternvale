//! One guest vCPU, bound to the thread that created it.
//!
//! `hv_vcpu.h` says each thread has one vCPU and that run, register access, and
//! destroy must happen on that thread. [`VcpuStop`] is the handle another thread
//! uses to call `hv_vcpus_exit`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::ThreadId;

use ternvale_hv::{
    Reg, SysReg, HV_EXIT_REASON_CANCELED, HV_EXIT_REASON_EXCEPTION, HV_EXIT_REASON_UNKNOWN,
    HV_EXIT_REASON_VTIMER_ACTIVATED,
};
use thiserror::Error;

/// Why `hv_vcpu_run` returned to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// Guest took a synchronous exception. `syndrome` is the raw ESR.
    Exception {
        /// Raw exception syndrome. Not decoded yet.
        syndrome: u64,
        /// FAR-style virtual address from the exit record.
        virtual_address: u64,
        /// Guest physical address from the exit record.
        physical_address: u64,
    },
    /// The virtual timer became pending. The PPI is already injected.
    VtimerActivated,
    /// PSCI `CPU_OFF`. x0 is 0 and PC is past the call.
    CpuOff,
    /// PSCI `SYSTEM_OFF`. The guest does not resume.
    SystemOff,
    /// PSCI `SYSTEM_RESET`. The guest does not resume.
    SystemReset,
    /// The guest executed `WFI` or `WFE`. PC is already past that instruction.
    /// The caller should poll host devices, then run again.
    Wfi,
    /// `hv_vcpus_exit` canceled the run.
    Canceled,
    /// A reason code that is not in `hv_vcpu_types.h`.
    Unknown {
        /// Raw `hv_exit_reason_t`.
        reason: u32,
    },
}

/// A vCPU operation failed, or it was used on the wrong thread.
#[derive(Debug, Error)]
pub enum VcpuError {
    /// Hypervisor.framework rejected the call.
    #[error("vcpu hypervisor call: {0}")]
    Hv(#[from] ternvale_hv::HvError),
    /// `run` or a register access was not on the creating thread.
    #[error("vcpu {id} belongs to thread {owner}, called from {caller}")]
    WrongThread {
        /// Kernel vCPU id.
        id: u64,
        /// Thread that called `hv_vcpu_create`.
        owner: String,
        /// Thread that made the rejected call.
        caller: String,
    },
}

/// Sends `hv_vcpus_exit` for one vCPU. This value is `Send`.
#[derive(Debug, Clone)]
pub struct VcpuStop {
    id: u64,
    stop: Arc<AtomicBool>,
    pending: Arc<AtomicBool>,
    wake: Arc<Condvar>,
}

impl VcpuStop {
    /// Mark the vCPU stopped and ask the kernel to cancel it.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn request(&self) -> Result<(), VcpuError> {
        self.stop.store(true, Ordering::Release);
        self.pending.store(true, Ordering::Release);
        self.wake.notify_one();
        tracing::info!(target: "ternvale::vcpu", vcpu_id = self.id, "stop requested");
        ternvale_hv::vcpus_exit(&[self.id])?;
        Ok(())
    }

    /// Cancel `hv_vcpu_run` without stopping the guest.
    ///
    /// The machine loop uses this to leave a hypervisor WFI and drain stdin.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn nudge(&self) -> Result<(), VcpuError> {
        self.pending.store(true, Ordering::Release);
        self.wake.notify_one();
        tracing::debug!(target: "ternvale::vcpu", vcpu_id = self.id, "vcpu nudge");
        ternvale_hv::vcpus_exit(&[self.id])?;
        Ok(())
    }

    /// Whether [`VcpuStop::request`] has run.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// A vCPU created and run on one thread.
pub struct Vcpu {
    pub(super) id: u64,
    /// Kernel `hv_vcpu_exit_t` pointer. Stored as `usize` so `Vcpu` is `Send`.
    /// The thread check rejects use from any thread but the owner.
    pub(super) exit: usize,
    owner: ThreadId,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) pending: Arc<AtomicBool>,
    pub(super) wake: Arc<Condvar>,
    pub(super) park: Arc<Mutex<()>>,
    /// Set after a VTIMER exit until `CNTV_CTL_EL0.ISTATUS` clears.
    pub(super) timer_masked: AtomicBool,
}

mod exit;

impl std::fmt::Debug for Vcpu {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Vcpu")
            .field("id", &self.id)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

impl Vcpu {
    /// Create a vCPU on this thread. `vm` must still be alive.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all)]
    pub fn create(vm: &ternvale_hv::Vm) -> Result<Self, VcpuError> {
        tracing::debug!(target: "ternvale::vcpu", vm = ?vm, "creating vCPU");
        let (id, exit) = ternvale_hv::vcpu_create()?;
        let exit = exit as usize;
        let owner = std::thread::current().id();
        tracing::info!(
            target: "ternvale::vcpu",
            vcpu_id = id,
            owner = ?owner,
            "vCPU bound to thread"
        );
        Ok(Self {
            id,
            exit,
            owner,
            stop: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(AtomicBool::new(false)),
            wake: Arc::new(Condvar::new()),
            park: Arc::new(Mutex::new(())),
            timer_masked: AtomicBool::new(false),
        })
    }

    /// Kernel vCPU id.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Handle another thread can use to cancel this vCPU.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn stopper(&self) -> VcpuStop {
        VcpuStop {
            id: self.id,
            stop: Arc::clone(&self.stop),
            pending: Arc::clone(&self.pending),
            wake: Arc::clone(&self.wake),
        }
    }

    /// Read `Xn`.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id, index))]
    pub fn get_x(&self, index: u8) -> Result<u64, VcpuError> {
        self.on_owner_thread()?;
        Ok(ternvale_hv::get_reg(self.id, Reg::X(index))?)
    }

    /// Write `Xn`.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id, index, value = format!("{:#x}", value)))]
    pub fn set_x(&self, index: u8, value: u64) -> Result<(), VcpuError> {
        self.on_owner_thread()?;
        ternvale_hv::set_reg(self.id, Reg::X(index), value)?;
        Ok(())
    }

    /// Read the program counter.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn get_pc(&self) -> Result<u64, VcpuError> {
        self.on_owner_thread()?;
        Ok(ternvale_hv::get_reg(self.id, Reg::Pc)?)
    }

    /// Write the program counter.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id, value = format!("{:#x}", value)))]
    pub fn set_pc(&self, value: u64) -> Result<(), VcpuError> {
        self.on_owner_thread()?;
        ternvale_hv::set_reg(self.id, Reg::Pc, value)?;
        Ok(())
    }

    /// Read CPSR.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn get_cpsr(&self) -> Result<u64, VcpuError> {
        self.on_owner_thread()?;
        Ok(ternvale_hv::get_reg(self.id, Reg::Cpsr)?)
    }

    /// Write CPSR.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id, value = format!("{:#x}", value)))]
    pub fn set_cpsr(&self, value: u64) -> Result<(), VcpuError> {
        self.on_owner_thread()?;
        ternvale_hv::set_reg(self.id, Reg::Cpsr, value)?;
        Ok(())
    }

    /// Read `SP_EL1`.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn get_sp_el1(&self) -> Result<u64, VcpuError> {
        self.on_owner_thread()?;
        Ok(ternvale_hv::get_sys_reg(self.id, SysReg::SpEl1)?)
    }

    /// Write `SP_EL1`.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id, value = format!("{:#x}", value)))]
    pub fn set_sp_el1(&self, value: u64) -> Result<(), VcpuError> {
        self.on_owner_thread()?;
        ternvale_hv::set_sys_reg(self.id, SysReg::SpEl1, value)?;
        Ok(())
    }

    /// Read a system register.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id, reg = ?reg))]
    pub fn get_sys_reg(&self, reg: SysReg) -> Result<u64, VcpuError> {
        self.on_owner_thread()?;
        Ok(ternvale_hv::get_sys_reg(self.id, reg)?)
    }

    /// Write a system register.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id, reg = ?reg, value = format!("{:#x}", value)))]
    pub fn set_sys_reg(&self, reg: SysReg, value: u64) -> Result<(), VcpuError> {
        self.on_owner_thread()?;
        ternvale_hv::set_sys_reg(self.id, reg, value)?;
        Ok(())
    }

    /// Run until the next exit the caller must handle.
    ///
    /// A virtual-timer exit is masked and injected as PPI 27, then the guest
    /// resumes. WFI parks briefly, then returns [`ExitReason::Wfi`] so the caller
    /// can poll devices. A recognized PSCI HVC or SMC is completed here. `SYSTEM_OFF`
    /// and `SYSTEM_RESET` return without resuming the guest.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn run(&self) -> Result<ExitReason, VcpuError> {
        self.on_owner_thread()?;
        if self.stop.load(Ordering::Acquire) {
            tracing::debug!(target: "ternvale::vcpu", vcpu_id = self.id, "run with stop flag set");
        }
        self.maybe_unmask_vtimer()?;
        loop {
            ternvale_hv::vcpu_run(self.id)?;
            let reason = self.read_exit();
            match self.dispatch(reason)? {
                exit::Loop::Again => {}
                exit::Loop::Done(reason) => return Ok(reason),
            }
        }
    }

    fn on_owner_thread(&self) -> Result<(), VcpuError> {
        let caller = std::thread::current().id();
        if caller != self.owner {
            tracing::error!(
                target: "ternvale::vcpu",
                vcpu_id = self.id,
                owner = ?self.owner,
                caller = ?caller,
                "rejected vCPU call from a different thread"
            );
            return Err(VcpuError::WrongThread {
                id: self.id,
                owner: format!("{:?}", self.owner),
                caller: format!("{:?}", caller),
            });
        }
        tracing::debug!(
            target: "ternvale::vcpu",
            vcpu_id = self.id,
            owner = ?self.owner,
            "vcpu call on owner thread"
        );
        Ok(())
    }
}

impl Drop for Vcpu {
    fn drop(&mut self) {
        if let Err(error) = self.on_owner_thread() {
            tracing::error!(target: "ternvale::vcpu", error = %error, "dropping vCPU off its owner thread");
        }
        if let Err(error) = ternvale_hv::vcpu_destroy(self.id) {
            tracing::error!(
                target: "ternvale::vcpu",
                vcpu_id = self.id,
                error = %error,
                "hv_vcpu_destroy failed"
            );
        } else {
            tracing::info!(target: "ternvale::vcpu", vcpu_id = self.id, "vCPU destroyed");
        }
    }
}

pub(crate) fn exit_reason(
    reason: u32,
    syndrome: u64,
    virtual_address: u64,
    physical_address: u64,
) -> ExitReason {
    match reason {
        HV_EXIT_REASON_EXCEPTION => ExitReason::Exception {
            syndrome,
            virtual_address,
            physical_address,
        },
        HV_EXIT_REASON_VTIMER_ACTIVATED => ExitReason::VtimerActivated,
        HV_EXIT_REASON_CANCELED => ExitReason::Canceled,
        HV_EXIT_REASON_UNKNOWN => ExitReason::Unknown {
            reason: HV_EXIT_REASON_UNKNOWN,
        },
        other => {
            tracing::error!(
                target: "ternvale::vcpu",
                reason = other,
                "unknown hv_vcpu exit reason"
            );
            ExitReason::Unknown { reason: other }
        }
    }
}

#[cfg(test)]
#[path = "vcpu_hv_test.rs"]
mod hv_test;

#[cfg(test)]
#[path = "vcpu_tests.rs"]
mod tests;
