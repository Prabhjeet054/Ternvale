//! Virtual timer, WFI, and PSCI exits inside [`super::Vcpu::run`].

use std::sync::atomic::Ordering;
use std::time::Duration;

use ternvale_hv::{SysReg, VcpuExit};

use super::{exit_reason, ExitReason, Vcpu, VcpuError};

pub(super) enum Loop {
    Again,
    Done(ExitReason),
}

impl Vcpu {
    pub(super) fn read_exit(&self) -> ExitReason {
        // SAFETY: `exit` was written by `hv_vcpu_create` and updated by `hv_vcpu_run`.
        // It stays allocated until `hv_vcpu_destroy`. The fields match `hv_vcpu_exit_t`.
        let exit = unsafe { &*(self.exit as *const VcpuExit) };
        exit_reason(
            exit.reason,
            exit.syndrome,
            exit.virtual_address,
            exit.physical_address,
        )
    }

    pub(super) fn dispatch(&self, reason: ExitReason) -> Result<Loop, VcpuError> {
        match reason {
            ExitReason::VtimerActivated => {
                self.on_vtimer()?;
                Ok(Loop::Again)
            }
            ExitReason::Exception {
                syndrome,
                physical_address,
                virtual_address,
            } => self.on_exception(syndrome, virtual_address, physical_address),
            other => {
                tracing::debug!(target: "ternvale::vcpu", vcpu_id = self.id, ?other, "vcpu exit");
                Ok(Loop::Done(other))
            }
        }
    }

    pub(super) fn maybe_unmask_vtimer(&self) -> Result<(), VcpuError> {
        if !self.timer_masked.load(Ordering::Acquire) {
            return Ok(());
        }
        let ctl = ternvale_hv::get_sys_reg(self.id, SysReg::CntvCtlEl0)?;
        const ISTATUS: u64 = 1 << 2;
        if ctl & ISTATUS != 0 {
            return Ok(());
        }
        ternvale_hv::set_vtimer_mask(self.id, false)?;
        self.timer_masked.store(false, Ordering::Release);
        tracing::debug!(
            target: "ternvale::gic",
            vcpu_id = self.id,
            "unmasked vtimer after ISTATUS cleared"
        );
        Ok(())
    }

    fn on_vtimer(&self) -> Result<(), VcpuError> {
        ternvale_hv::set_vtimer_mask(self.id, true)?;
        ternvale_hv::raise_ppi(self.id, ternvale_hv::VTIMER_PPI)?;
        self.timer_masked.store(true, Ordering::Release);
        tracing::info!(
            target: "ternvale::gic",
            vcpu_id = self.id,
            intid = ternvale_hv::VTIMER_PPI,
            "vtimer exit masked and PPI injected"
        );
        Ok(())
    }

    fn on_exception(
        &self,
        syndrome: u64,
        virtual_address: u64,
        physical_address: u64,
    ) -> Result<Loop, VcpuError> {
        tracing::trace!(
            target: "ternvale::vcpu",
            vcpu_id = self.id,
            syndrome = format!("{:#x}", syndrome),
            "vcpu exception exit"
        );
        let event = crate::esr::decode(syndrome, physical_address);
        match event {
            crate::esr::ExitEvent::Hvc { .. } => {
                self.on_psci(syndrome, virtual_address, physical_address, false)
            }
            crate::esr::ExitEvent::Smc { .. } => {
                self.on_psci(syndrome, virtual_address, physical_address, true)
            }
            crate::esr::ExitEvent::Wfi { wfe } => {
                self.wait_for_interrupt(wfe)?;
                crate::psci::advance_pc(self)?;
                Ok(Loop::Again)
            }
            _ => Ok(Loop::Done(ExitReason::Exception {
                syndrome,
                virtual_address,
                physical_address,
            })),
        }
    }

    fn on_psci(
        &self,
        syndrome: u64,
        virtual_address: u64,
        physical_address: u64,
        advance: bool,
    ) -> Result<Loop, VcpuError> {
        let x0 = self.get_x(0)?;
        let x1 = self.get_x(1)?;
        let x2 = self.get_x(2)?;
        let x3 = self.get_x(3)?;
        let Some(action) = crate::psci::call(x0, x1, x2, x3) else {
            return Ok(Loop::Done(ExitReason::Exception {
                syndrome,
                virtual_address,
                physical_address,
            }));
        };
        match action {
            crate::psci::PsciAction::Return { x0 } => {
                self.set_x(0, x0)?;
                if advance {
                    crate::psci::advance_pc(self)?;
                }
                Ok(Loop::Again)
            }
            crate::psci::PsciAction::CpuOff => {
                self.set_x(0, 0)?;
                if advance {
                    crate::psci::advance_pc(self)?;
                }
                self.stop.store(true, Ordering::Release);
                Ok(Loop::Done(ExitReason::CpuOff))
            }
            crate::psci::PsciAction::SystemOff => {
                self.stop.store(true, Ordering::Release);
                tracing::info!(target: "ternvale::psci", vcpu_id = self.id, "system off");
                Ok(Loop::Done(ExitReason::SystemOff))
            }
            crate::psci::PsciAction::SystemReset => {
                self.stop.store(true, Ordering::Release);
                tracing::info!(target: "ternvale::psci", vcpu_id = self.id, "system reset");
                Ok(Loop::Done(ExitReason::SystemReset))
            }
        }
    }

    fn wait_for_interrupt(&self, wfe: bool) -> Result<(), VcpuError> {
        if wfe {
            tracing::debug!(target: "ternvale::vcpu", vcpu_id = self.id, "wfe yield");
            std::thread::yield_now();
            return Ok(());
        }
        tracing::debug!(target: "ternvale::vcpu", vcpu_id = self.id, "wfi park");
        let guard = self
            .park
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if self.stop.load(Ordering::Acquire) || self.pending.swap(false, Ordering::AcqRel) {
            tracing::debug!(
                target: "ternvale::vcpu",
                vcpu_id = self.id,
                "wfi canceled before sleep"
            );
            return Ok(());
        }
        let (_guard, wait) = self
            .wake
            .wait_timeout(guard, Duration::from_millis(10))
            .unwrap_or_else(|poison| poison.into_inner());
        self.pending.store(false, Ordering::Release);
        tracing::debug!(
            target: "ternvale::vcpu",
            vcpu_id = self.id,
            timed_out = wait.timed_out(),
            "wfi woke"
        );
        Ok(())
    }
}
