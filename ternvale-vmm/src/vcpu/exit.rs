//! Virtual timer, WFI, and PSCI exits inside [`super::Vcpu::run`].

use std::sync::atomic::Ordering;

use ternvale_hv::{SysReg, VcpuExit};

use super::{exit_reason, ExitReason, Vcpu, VcpuError};
#[cfg(feature = "metrics")]
use ternvale_metrics::ExitKind;

pub(super) enum Loop {
    Again,
    Done(ExitReason),
}

/// Map a hypervisor exit onto a metrics class without calling [`crate::esr::decode`]
/// (that path logs and would run twice per exit).
#[cfg(feature = "metrics")]
#[inline]
pub(super) fn kind(reason: ExitReason) -> ExitKind {
    match reason {
        ExitReason::VtimerActivated => ExitKind::Vtimer,
        ExitReason::Canceled => ExitKind::Canceled,
        ExitReason::Wfi => ExitKind::Wfi,
        ExitReason::Exception { syndrome, .. } => kind_from_esr(syndrome),
        ExitReason::Unknown { .. } => ExitKind::Unknown,
        _ => ExitKind::Other,
    }
}

#[cfg(all(test, feature = "metrics"))]
mod kind_tests {
    use super::{kind, kind_from_esr};
    use crate::vcpu::ExitReason;
    use ternvale_metrics::ExitKind;

    fn esr(ec: u64, iss: u64) -> u64 {
        (ec << 26) | iss
    }

    #[test]
    fn esr_classes_match_exit_event_kinds() {
        assert_eq!(kind_from_esr(esr(0x01, 0)), ExitKind::Wfi);
        assert_eq!(kind_from_esr(esr(0x01, 1)), ExitKind::Wfe);
        assert_eq!(kind_from_esr(esr(0x16, 0x42)), ExitKind::Hvc);
        assert_eq!(kind_from_esr(esr(0x17, 7)), ExitKind::Smc);
        assert_eq!(kind_from_esr(esr(0x18, 0)), ExitKind::SysReg);
        assert_eq!(kind_from_esr(esr(0x24, 1 << 24)), ExitKind::Mmio);
        assert_eq!(kind_from_esr(esr(0x24, 0)), ExitKind::Unknown);
        assert_eq!(kind(ExitReason::VtimerActivated), ExitKind::Vtimer);
    }
}

/// EC/ISS only (ARM ARM DDI 0487): same classes as [`crate::esr::decode`].
#[cfg(feature = "metrics")]
#[inline]
fn kind_from_esr(esr: u64) -> ExitKind {
    let ec = (esr >> 26) & 0x3f;
    let iss = esr & 0x1ff_ffff;
    match ec {
        0x01 if iss & 1 == 1 => ExitKind::Wfe,
        0x01 => ExitKind::Wfi,
        0x16 => ExitKind::Hvc,
        0x17 => ExitKind::Smc,
        0x18 => ExitKind::SysReg,
        0x24 | 0x25 if (iss >> 24) & 1 == 1 => ExitKind::Mmio,
        _ => ExitKind::Unknown,
    }
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
        self.counters.vtimer();
        #[cfg(feature = "metrics")]
        if let Some((metrics, _)) = self.metrics.get() {
            metrics.record_interrupt();
        }
        tracing::trace!(
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
        if tracing::enabled!(target: "ternvale::vcpu", tracing::Level::TRACE) {
            tracing::trace!(
                target: "ternvale::vcpu",
                vcpu_id = self.id,
                syndrome = format!("{:#x}", syndrome),
                "vcpu exception exit"
            );
        }
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
                // Return so the machine loop can drain host devices. Stdin and
                // SPI re-pulses already `nudge` the vCPU; this is not a poll
                // cap on the vCPU thread.
                Ok(Loop::Done(ExitReason::Wfi))
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
            crate::psci::PsciAction::Power(request) => {
                if advance {
                    crate::psci::advance_pc(self)?;
                }
                Ok(Loop::Done(ExitReason::Psci(request)))
            }
            crate::psci::PsciAction::CpuOff => {
                self.set_x(0, 0)?;
                if advance {
                    crate::psci::advance_pc(self)?;
                }
                tracing::info!(target: "ternvale::psci", vcpu_id = self.id, "cpu off");
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

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::vcpu",
        skip_all,
        fields(vcpu_id = self.id, wfe)
    )]
    fn wait_for_interrupt(&self, wfe: bool) -> Result<(), VcpuError> {
        let ctl = ternvale_hv::get_sys_reg(self.id, SysReg::CntvCtlEl0)?;
        let cval = ternvale_hv::get_sys_reg(self.id, SysReg::CntvCvalEl0)?;
        let offset = ternvale_hv::get_vtimer_offset(self.id)?;
        let cntvct = ternvale_hv::host_ticks().saturating_sub(offset);
        let park = super::wfi::park_until(ctl, cval, cntvct, ternvale_hv::ticks_to_duration);
        tracing::debug!(
            target: "ternvale::vcpu",
            vcpu_id = self.id,
            wfe,
            ctl = %format!("{ctl:#x}"),
            cval = %format!("{cval:#x}"),
            cntvct = %format!("{cntvct:#x}"),
            ?park,
            "wfi/wfe park decision"
        );
        if matches!(park, super::wfi::Park::Ready) {
            return Ok(());
        }
        let guard = crate::lockwatch::lock(&self.park, "wfi-park");
        if self.stop.load(Ordering::Acquire) || self.pending.swap(false, Ordering::AcqRel) {
            tracing::debug!(
                target: "ternvale::vcpu",
                vcpu_id = self.id,
                "wfi canceled before sleep"
            );
            return Ok(());
        }
        let parked = std::time::Instant::now();
        let timed_out = if let super::wfi::Park::Until(wait) = park {
            let (_guard, timed_out) = guard.wait_timeout(&self.wake, wait);
            timed_out
        } else {
            let _guard = guard.park(&self.wake);
            false
        };
        self.counters.park(parked.elapsed());
        self.pending.store(false, Ordering::Release);
        tracing::debug!(
            target: "ternvale::vcpu",
            vcpu_id = self.id,
            timed_out,
            parked_us = parked.elapsed().as_micros() as u64,
            "wfi woke"
        );
        Ok(())
    }
}
