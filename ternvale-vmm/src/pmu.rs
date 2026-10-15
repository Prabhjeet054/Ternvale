//! Advertise a PMUv3 so Hypervisor.framework emulates the PMU registers.
//!
//! The framework's default `ID_AA64DFR0_EL1` has `PMUVer` = 0, and with that it
//! injects an Undefined Instruction exception into the guest for every PMU
//! register access instead of exiting to the VMM (measured on macOS 27 by
//! `pmu_hv_test`; `hv_vm_config.h` documents this for EL2-enabled VMs only).
//! Windows boot applications read `PMCR_EL0` unconditionally, so `cdboot.efi`
//! halted in its own vector before printing anything. With a non-zero `PMUVer`
//! the framework emulates the registers itself and the read completes.
//!
//! `ID_AA64DFR0_EL1.PMUVer` is bits \[11:8\]; `0b0001` is "PMUv3 implemented"
//! (ARM ARM DDI 0487, ID_AA64DFR0_EL1 field descriptions). TODO(verify): which
//! PMUv3 extensions the framework emulates; only plain PMUv3 is advertised.

use ternvale_hv::SysReg;

use crate::vcpu::{Vcpu, VcpuError};

const PMUVER_SHIFT: u32 = 8;
const PMUVER_MASK: u64 = 0xf << PMUVER_SHIFT;
const PMUVER_PMUV3: u64 = 0b0001;
/// `0b1111`: an IMPLEMENTATION DEFINED PMU, which the framework does not emulate.
const PMUVER_IMPDEF: u64 = 0b1111;

/// `dfr0` with `PMUVer` set to PMUv3 when it reports no architected PMU.
/// An existing PMUv3 version is kept.
pub(crate) fn with_pmuv3(dfr0: u64) -> u64 {
    match (dfr0 & PMUVER_MASK) >> PMUVER_SHIFT {
        0 | PMUVER_IMPDEF => (dfr0 & !PMUVER_MASK) | (PMUVER_PMUV3 << PMUVER_SHIFT),
        _ => dfr0,
    }
}

/// Make this vCPU report PMUv3 before its first run. Must run on the vCPU's thread.
#[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = vcpu.id()))]
pub(crate) fn advertise_pmuv3(vcpu: &Vcpu) -> Result<(), VcpuError> {
    let before = vcpu.get_sys_reg(SysReg::IdAa64Dfr0El1)?;
    let after = with_pmuv3(before);
    if after != before {
        vcpu.set_sys_reg(SysReg::IdAa64Dfr0El1, after)?;
    }
    tracing::debug!(
        target: "ternvale::vcpu",
        vcpu_id = vcpu.id(),
        before = format!("{before:#x}"),
        after = format!("{after:#x}"),
        "ID_AA64DFR0_EL1 advertises PMUv3"
    );
    Ok(())
}

#[cfg(test)]
#[path = "pmu_tests.rs"]
mod tests;
