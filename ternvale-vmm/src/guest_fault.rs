//! A stopped vCPU's last guest state, so a guest that parked itself in its own
//! exception vector (a `b .` handler, as the Windows boot applications use)
//! shows the exception it took instead of an unexplained spin.
//!
//! The vector layout is the ARM ARM's "Exception vectors" table (DDI 0487,
//! D1.3.1 in recent issues; the section number moves between issues,
//! TODO(verify) against the issue you cite): `VBAR_EL1` is 2 KiB aligned, and
//! each of the 16 entries is 0x80 bytes. The ESR exception classes are from the
//! `ESR_ELx.EC` field description (D24.2.x, "ESR_EL1").

use ternvale_hv::SysReg;

use crate::vcpu::{Vcpu, VcpuError};

const VECTOR_TABLE_BYTES: u64 = 0x800;
const VECTOR_ENTRY_BYTES: u64 = 0x80;

/// The 16 entries of an AArch64 vector table, in offset order.
const VECTORS: [&str; 16] = [
    "synchronous, current EL, SP_EL0",
    "irq, current EL, SP_EL0",
    "fiq, current EL, SP_EL0",
    "serror, current EL, SP_EL0",
    "synchronous, current EL, SP_ELx",
    "irq, current EL, SP_ELx",
    "fiq, current EL, SP_ELx",
    "serror, current EL, SP_ELx",
    "synchronous, lower EL, AArch64",
    "irq, lower EL, AArch64",
    "fiq, lower EL, AArch64",
    "serror, lower EL, AArch64",
    "synchronous, lower EL, AArch32",
    "irq, lower EL, AArch32",
    "fiq, lower EL, AArch32",
    "serror, lower EL, AArch32",
];

/// The vector entry `pc` is in, when it is inside the table at `vbar`.
pub(crate) fn vector_name(vbar: u64, pc: u64) -> Option<&'static str> {
    let offset = pc.checked_sub(vbar & !(VECTOR_TABLE_BYTES - 1))?;
    if offset >= VECTOR_TABLE_BYTES {
        return None;
    }
    VECTORS.get((offset / VECTOR_ENTRY_BYTES) as usize).copied()
}

/// `ESR_ELx.EC` (bits 31:26) as a name.
pub(crate) fn exception_class(esr: u64) -> &'static str {
    match (esr >> 26) & 0x3f {
        0x00 => "unknown reason",
        0x01 => "wfi/wfe trap",
        0x07 => "sve/simd/fp access trap",
        0x0e => "illegal execution state",
        0x15 => "svc (aarch64)",
        0x16 => "hvc (aarch64)",
        0x17 => "smc (aarch64)",
        0x18 => "msr/mrs/system instruction trap",
        0x19 => "sve access trap",
        0x20 => "instruction abort, lower EL",
        0x21 => "instruction abort, same EL",
        0x22 => "pc alignment fault",
        0x24 => "data abort, lower EL",
        0x25 => "data abort, same EL",
        0x26 => "sp alignment fault",
        0x2c => "floating-point exception",
        0x2f => "serror",
        0x30 | 0x31 => "breakpoint",
        0x32 | 0x33 => "software step",
        0x34 | 0x35 => "watchpoint",
        0x3c => "brk instruction",
        _ => "other",
    }
}

/// Log the stopped vCPU's PC and EL1 exception registers at INFO, and WARN when
/// the PC is inside the guest's own vector table. Must run on the vCPU's thread.
#[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(cpu = index))]
pub(crate) fn log_final_state(index: u32, vcpu: &Vcpu) -> Result<(), VcpuError> {
    let pc = vcpu.get_pc()?;
    let vbar = vcpu.get_sys_reg(SysReg::VbarEl1)?;
    let esr = vcpu.get_sys_reg(SysReg::EsrEl1)?;
    let elr = vcpu.get_sys_reg(SysReg::ElrEl1)?;
    let far = vcpu.get_sys_reg(SysReg::FarEl1)?;
    let spsr = vcpu.get_sys_reg(SysReg::SpsrEl1)?;
    let hex = |value: u64| format!("{value:#x}");
    tracing::info!(
        target: "ternvale::vcpu",
        cpu = index,
        pc = hex(pc),
        vbar_el1 = hex(vbar),
        esr_el1 = hex(esr),
        elr_el1 = hex(elr),
        far_el1 = hex(far),
        spsr_el1 = hex(spsr),
        "vcpu final guest state"
    );
    if let Some(vector) = vector_name(vbar, pc) {
        tracing::warn!(
            target: "ternvale::vcpu",
            cpu = index,
            pc = hex(pc),
            vector,
            exception = exception_class(esr),
            esr_el1 = hex(esr),
            elr_el1 = hex(elr),
            far_el1 = hex(far),
            "guest stopped inside its own exception vector; ESR/ELR/FAR describe the exception it took"
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "guest_fault_tests.rs"]
mod tests;
