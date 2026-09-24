//! One guest vCPU, bound to the thread that created it.
//!
//! `hv_vcpu.h` says each thread has one vCPU and that run, register access, and
//! destroy must happen on that thread. [`VcpuStop`] is the handle another thread
//! uses to call `hv_vcpus_exit`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::ThreadId;

use ternvale_hv::{
    Reg, SysReg, VcpuExit, HV_EXIT_REASON_CANCELED, HV_EXIT_REASON_EXCEPTION,
    HV_EXIT_REASON_UNKNOWN, HV_EXIT_REASON_VTIMER_ACTIVATED,
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
    /// The virtual timer became pending.
    VtimerActivated,
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
}

impl VcpuStop {
    /// Mark the vCPU stopped and ask the kernel to cancel it.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn request(&self) -> Result<(), VcpuError> {
        self.stop.store(true, Ordering::Release);
        tracing::info!(target: "ternvale::vcpu", vcpu_id = self.id, "stop requested");
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
    id: u64,
    /// Kernel `hv_vcpu_exit_t` pointer. Stored as `usize` so `Vcpu` is `Send`.
    /// The thread check rejects use from any thread but the owner.
    exit: usize,
    owner: ThreadId,
    stop: Arc<AtomicBool>,
}

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
        })
    }

    /// Handle another thread can use to cancel this vCPU.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn stopper(&self) -> VcpuStop {
        VcpuStop {
            id: self.id,
            stop: Arc::clone(&self.stop),
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

    /// Run until the next exit.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn run(&self) -> Result<ExitReason, VcpuError> {
        self.on_owner_thread()?;
        if self.stop.load(Ordering::Acquire) {
            tracing::debug!(target: "ternvale::vcpu", vcpu_id = self.id, "run with stop flag set");
        }
        ternvale_hv::vcpu_run(self.id)?;
        // SAFETY: `exit` was written by `hv_vcpu_create` and updated by `hv_vcpu_run`.
        // It stays allocated until `hv_vcpu_destroy`. The fields match `hv_vcpu_exit_t`.
        let exit = unsafe { &*(self.exit as *const VcpuExit) };
        let reason = exit_reason(
            exit.reason,
            exit.syndrome,
            exit.virtual_address,
            exit.physical_address,
        );
        if let ExitReason::Exception { syndrome, .. } = reason {
            tracing::trace!(
                target: "ternvale::vcpu",
                vcpu_id = self.id,
                syndrome = format!("{:#x}", syndrome),
                "vcpu exception exit"
            );
        }
        tracing::debug!(target: "ternvale::vcpu", vcpu_id = self.id, ?reason, "vcpu exit");
        Ok(reason)
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
mod tests {
    use super::{exit_reason, ExitReason};
    use ternvale_hv::{
        HV_EXIT_REASON_CANCELED, HV_EXIT_REASON_EXCEPTION, HV_EXIT_REASON_UNKNOWN,
        HV_EXIT_REASON_VTIMER_ACTIVATED,
    };

    #[test]
    fn maps_exit_reasons_without_a_vm() {
        let exception = exit_reason(HV_EXIT_REASON_EXCEPTION, 0x5600_0000, 0x10, 0x4000_0000);
        assert_eq!(
            exception,
            ExitReason::Exception {
                syndrome: 0x5600_0000,
                virtual_address: 0x10,
                physical_address: 0x4000_0000,
            }
        );
        assert_eq!(
            exit_reason(HV_EXIT_REASON_VTIMER_ACTIVATED, 0, 0, 0),
            ExitReason::VtimerActivated
        );
        assert_eq!(
            exit_reason(HV_EXIT_REASON_CANCELED, 0, 0, 0),
            ExitReason::Canceled
        );
        assert_eq!(
            exit_reason(HV_EXIT_REASON_UNKNOWN, 0, 0, 0),
            ExitReason::Unknown {
                reason: HV_EXIT_REASON_UNKNOWN
            }
        );
        assert_eq!(
            exit_reason(0x99, 0, 0, 0),
            ExitReason::Unknown { reason: 0x99 }
        );
    }

    #[test]
    fn rejects_a_gpr_index_past_x30() {
        let error = ternvale_hv::Reg::X(31);
        let message = match ternvale_hv::get_reg(0, error) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("x31 was accepted"),
        };
        assert!(message.contains("31"), "{message}");
    }

    #[test]
    #[ignore = "needs-hv"]
    fn creates_vcpu_and_rejects_another_thread() {
        let vm = ternvale_hv::Vm::create().expect("vm");
        let vcpu = super::Vcpu::create(&vm).expect("vcpu");
        vcpu.set_pc(0x4000_0000).expect("set pc");
        assert_eq!(vcpu.get_pc().expect("get pc"), 0x4000_0000);
        vcpu.set_x(0, 0x11).expect("set x0");
        assert_eq!(vcpu.get_x(0).expect("get x0"), 0x11);
        vcpu.set_x(30, 0x1e).expect("set x30");
        assert_eq!(vcpu.get_x(30).expect("get x30"), 0x1e);
        vcpu.set_sp_el1(0x8000).expect("set sp");
        assert_eq!(vcpu.get_sp_el1().expect("get sp"), 0x8000);
        vcpu.set_cpsr(0x3c5).expect("set cpsr");
        assert_eq!(vcpu.get_cpsr().expect("get cpsr"), 0x3c5);
        vcpu.set_sys_reg(ternvale_hv::SysReg::VbarEl1, 0x1000)
            .expect("set vbar");
        assert_eq!(
            vcpu.get_sys_reg(ternvale_hv::SysReg::VbarEl1)
                .expect("get vbar"),
            0x1000
        );
        let stop = vcpu.stopper();
        let stopped = std::thread::spawn(move || {
            stop.request().expect("hv_vcpus_exit");
            stop.is_stopped()
        })
        .join()
        .expect("stop thread");
        assert!(stopped);
        let wrong =
            std::thread::scope(|scope| scope.spawn(|| vcpu.get_pc()).join().expect("other thread"));
        let message = wrong.expect_err("other thread should fail").to_string();
        assert!(message.contains("belongs to thread"), "{message}");
        drop(vcpu);
        drop(vm);
    }
}
