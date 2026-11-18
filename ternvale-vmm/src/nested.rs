//! Nested virt: apply trapped EL2 `MRS`/`MSR` via Hypervisor.framework.
//!
//! Apple documents `HV_SYS_REG_*_EL2` (`hv_vcpu_types.h`, macos(15.0)) as valid
//! only after `hv_vm_config_set_el2_enabled`. ICH / `ICC_SRE_EL2` use the GIC
//! APIs in `hv_gic.h`, not `hv_vcpu_set_sys_reg`.

use ternvale_hv::{sys_reg_encoding, Gic};

use crate::vcpu::{Vcpu, VcpuError};

/// `HV_SYS_REG_*_EL2` values from `hv_vcpu_types.h`, sorted for `binary_search`.
const HV_EL2_SYSREGS: &[u16] = &[
    0xe000, // VPIDR_EL2
    0xe005, // VMPIDR_EL2
    0xe080, // SCTLR_EL2
    0xe088, // HCR_EL2
    0xe089, // MDCR_EL2
    0xe08a, // CPTR_EL2
    0xe100, // TTBR0_EL2
    0xe101, // TTBR1_EL2
    0xe102, // TCR_EL2
    0xe108, // VTTBR_EL2
    0xe10a, // VTCR_EL2
    0xe200, // SPSR_EL2
    0xe201, // ELR_EL2
    0xe290, // ESR_EL2
    0xe300, // FAR_EL2
    0xe304, // HPFAR_EL2
    0xe510, // MAIR_EL2
    0xe600, // VBAR_EL2
    0xe682, // TPIDR_EL2
    0xe703, // CNTVOFF_EL2
    0xe708, // CNTHCTL_EL2
    0xe710, // CNTHP_TVAL_EL2
    0xe711, // CNTHP_CTL_EL2
    0xe712, // CNTHP_CVAL_EL2
    0xf208, // SP_EL2
];

/// `HV_GIC_ICC_REG_SRE_EL2`.
const ICC_SRE_EL2: u16 = 0xe64d;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum El2Kind {
    Sys,
    Ich,
    Icc,
}

/// Fields from a trapped `MRS`/`MSR` (ESR EC `0x18`).
#[derive(Debug, Clone, Copy)]
pub struct El2SysTrap {
    /// `Op0`.
    pub op0: u8,
    /// `Op1`.
    pub op1: u8,
    /// `CRn`.
    pub crn: u8,
    /// `CRm`.
    pub crm: u8,
    /// `Op2`.
    pub op2: u8,
    /// Transfer register `Rt`.
    pub rt: u8,
    /// Guest used `MSR`.
    pub write: bool,
}

/// Apply a trapped EL2 system-register access. `true` when Hypervisor.framework
/// handled it; `false` when the encoding is unknown (caller skips).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::nested",
    skip_all,
    fields(
        vcpu_id = vcpu.id(),
        op0 = trap.op0,
        op1 = trap.op1,
        crn = trap.crn,
        crm = trap.crm,
        op2 = trap.op2,
        write = trap.write,
        rt = trap.rt
    )
)]
pub fn apply_el2_sysreg(vcpu: &Vcpu, gic: &Gic, trap: El2SysTrap) -> Result<bool, VcpuError> {
    let enc = sys_reg_encoding(trap.op0, trap.op1, trap.crn, trap.crm, trap.op2);
    let Some(kind) = classify(enc) else {
        return Ok(false);
    };
    if trap.write {
        let value = if trap.rt <= 30 {
            vcpu.get_x(trap.rt)?
        } else {
            0
        };
        match kind {
            El2Kind::Sys => vcpu.set_sys_reg_enc(enc, value)?,
            El2Kind::Ich => gic.set_ich_enc(vcpu.id(), enc, value)?,
            El2Kind::Icc => gic.set_icc_enc(vcpu.id(), enc, value)?,
        }
        if tracing::enabled!(target: "ternvale::nested", tracing::Level::DEBUG) {
            tracing::debug!(
                target: "ternvale::nested",
                enc = format!("{enc:#x}"),
                value = format!("{value:#x}"),
                ?kind,
                "EL2 MSR applied"
            );
        }
    } else {
        let value = match kind {
            El2Kind::Sys => vcpu.get_sys_reg_enc(enc)?,
            El2Kind::Ich => gic.get_ich_enc(vcpu.id(), enc)?,
            El2Kind::Icc => gic.get_icc_enc(vcpu.id(), enc)?,
        };
        if trap.rt <= 30 {
            vcpu.set_x(trap.rt, value)?;
        }
        if tracing::enabled!(target: "ternvale::nested", tracing::Level::DEBUG) {
            tracing::debug!(
                target: "ternvale::nested",
                enc = format!("{enc:#x}"),
                value = format!("{value:#x}"),
                ?kind,
                "EL2 MRS applied"
            );
        }
    }
    Ok(true)
}

fn classify(enc: u16) -> Option<El2Kind> {
    if HV_EL2_SYSREGS.binary_search(&enc).is_ok() {
        return Some(El2Kind::Sys);
    }
    if is_ich(enc) {
        return Some(El2Kind::Ich);
    }
    if enc == ICC_SRE_EL2 {
        return Some(El2Kind::Icc);
    }
    None
}

/// ICH encodings from `hv_gic_types.h` (`AP0R0`, `AP1R0`, `HCR`…`VMCR`, `LR0`…`LR15`).
fn is_ich(enc: u16) -> bool {
    matches!(
        enc,
        0xe640 | 0xe648 | 0xe658..=0xe65b | 0xe65d | 0xe65f | 0xe660..=0xe66f
    )
}

#[cfg(test)]
mod tests {
    use super::{classify, El2Kind, HV_EL2_SYSREGS, ICC_SRE_EL2};
    use ternvale_hv::sys_reg_encoding;

    #[test]
    fn classifies_cnthctl_and_ich_hcr() {
        let cnthctl = sys_reg_encoding(3, 4, 14, 1, 0);
        assert_eq!(cnthctl, 0xe708);
        assert_eq!(classify(cnthctl), Some(El2Kind::Sys));
        assert_eq!(
            classify(sys_reg_encoding(3, 6, 4, 1, 0)),
            Some(El2Kind::Sys)
        );
        assert_eq!(
            classify(sys_reg_encoding(3, 4, 12, 11, 0)),
            Some(El2Kind::Ich)
        );
        assert_eq!(classify(ICC_SRE_EL2), Some(El2Kind::Icc));
        assert_eq!(classify(sys_reg_encoding(3, 0, 1, 0, 0)), None);
        let mut sorted = HV_EL2_SYSREGS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.as_slice(), HV_EL2_SYSREGS);
    }
}
