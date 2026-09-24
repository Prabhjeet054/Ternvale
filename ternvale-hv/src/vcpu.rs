//! vCPU create, registers, run, and `hv_vcpus_exit`.
//!
//! Register numbers are from `hv_vcpu_types.h` in the macOS 27 SDK.
//! `HV_REG_X0` is 0, `HV_REG_X30` is 30, `HV_REG_PC` is 31, `HV_REG_CPSR` is 34.
//! `HV_SYS_REG_SP_EL1` is `0xe208`. Exit reasons start at 0: canceled, exception,
//! vtimer, unknown. `hv_vcpu_exit_t` is 32 bytes with the exception at offset 8.

use crate::error::{HvError, HV_SUCCESS};
use crate::ffi::{
    hv_vcpu_create, hv_vcpu_destroy, hv_vcpu_get_reg, hv_vcpu_get_sys_reg, hv_vcpu_run,
    hv_vcpu_set_reg, hv_vcpu_set_sys_reg, hv_vcpus_exit,
};

pub use crate::ffi::VcpuExit;

/// `HV_EXIT_REASON_CANCELED`.
pub const HV_EXIT_REASON_CANCELED: u32 = 0;
/// `HV_EXIT_REASON_EXCEPTION`.
pub const HV_EXIT_REASON_EXCEPTION: u32 = 1;
/// `HV_EXIT_REASON_VTIMER_ACTIVATED`.
pub const HV_EXIT_REASON_VTIMER_ACTIVATED: u32 = 2;
/// `HV_EXIT_REASON_UNKNOWN`.
pub const HV_EXIT_REASON_UNKNOWN: u32 = 3;

const REG_PC: u32 = 31;
const REG_CPSR: u32 = 34;

/// A general-purpose register, the PC, or CPSR (`hv_reg_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg {
    /// `X0` through `X30`. The index must be `0..=30`.
    X(u8),
    /// `HV_REG_PC`.
    Pc,
    /// `HV_REG_CPSR`.
    Cpsr,
}

impl Reg {
    fn code(self) -> Result<u32, HvError> {
        match self {
            Self::X(index) if index <= 30 => Ok(u32::from(index)),
            Self::X(index) => {
                tracing::warn!(
                    target: "ternvale::hv",
                    index,
                    "rejected gpr index"
                );
                Err(HvError::InvalidGpr { index })
            }
            Self::Pc => Ok(REG_PC),
            Self::Cpsr => Ok(REG_CPSR),
        }
    }
}

/// System registers used to boot a guest (`hv_sys_reg_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysReg {
    /// `HV_SYS_REG_SP_EL1` (`0xe208`).
    SpEl1,
    /// `HV_SYS_REG_SCTLR_EL1` (`0xc080`).
    SctlrEl1,
    /// `HV_SYS_REG_TTBR0_EL1` (`0xc100`).
    Ttbr0El1,
    /// `HV_SYS_REG_TTBR1_EL1` (`0xc101`).
    Ttbr1El1,
    /// `HV_SYS_REG_TCR_EL1` (`0xc102`).
    TcrEl1,
    /// `HV_SYS_REG_MAIR_EL1` (`0xc510`).
    MairEl1,
    /// `HV_SYS_REG_VBAR_EL1` (`0xc600`).
    VbarEl1,
    /// `HV_SYS_REG_ELR_EL1` (`0xc201`).
    ElrEl1,
    /// `HV_SYS_REG_ESR_EL1` (`0xc290`).
    EsrEl1,
    /// `HV_SYS_REG_FAR_EL1` (`0xc300`).
    FarEl1,
    /// `HV_SYS_REG_MPIDR_EL1` (`0xc005`).
    MpidrEl1,
    /// `HV_SYS_REG_CNTV_CTL_EL0` (`0xdf19`).
    CntvCtlEl0,
    /// `HV_SYS_REG_CNTV_CVAL_EL0` (`0xdf1a`).
    CntvCvalEl0,
}

impl SysReg {
    fn code(self) -> u16 {
        match self {
            Self::SpEl1 => 0xe208,
            Self::SctlrEl1 => 0xc080,
            Self::Ttbr0El1 => 0xc100,
            Self::Ttbr1El1 => 0xc101,
            Self::TcrEl1 => 0xc102,
            Self::MairEl1 => 0xc510,
            Self::VbarEl1 => 0xc600,
            Self::ElrEl1 => 0xc201,
            Self::EsrEl1 => 0xc290,
            Self::FarEl1 => 0xc300,
            Self::MpidrEl1 => 0xc005,
            Self::CntvCtlEl0 => 0xdf19,
            Self::CntvCvalEl0 => 0xdf1a,
        }
    }
}

/// Create a vCPU on the current thread. The exit pointer is owned by the kernel.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all)]
pub fn vcpu_create() -> Result<(u64, *mut VcpuExit), HvError> {
    let mut id = 0u64;
    let mut exit = std::ptr::null_mut();
    // SAFETY: `id` and `exit` are writable locals. `config` is NULL, which the
    // header allows. The kernel writes the exit object and keeps it until destroy.
    let raw = unsafe { hv_vcpu_create(&mut id, &mut exit, std::ptr::null_mut()) };
    let code = ternvale_log::log_hv_call!("hv_vcpu_create", format!("id={id:#x}"), raw);
    if code != HV_SUCCESS {
        let error = HvError::from_code(code);
        tracing::error!(target: "ternvale::hv", error = %error, "hv_vcpu_create failed");
        return Err(error);
    }
    if exit.is_null() {
        tracing::error!(target: "ternvale::hv", id, "hv_vcpu_create returned a null exit pointer");
        if let Err(error) = vcpu_destroy(id) {
            tracing::error!(target: "ternvale::hv", error = %error, "hv_vcpu_destroy after null exit failed");
        }
        return Err(HvError::Unknown { code: 0 });
    }
    tracing::info!(target: "ternvale::hv", vcpu_id = id, "vCPU created");
    Ok((id, exit))
}

/// Destroy a vCPU. Must run on the thread that created it.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all, fields(vcpu_id = id))]
pub fn vcpu_destroy(id: u64) -> Result<(), HvError> {
    // SAFETY: `id` came from a successful `hv_vcpu_create` and is destroyed once.
    let raw = unsafe { hv_vcpu_destroy(id) };
    let code = ternvale_log::log_hv_call!("hv_vcpu_destroy", format!("id={id:#x}"), raw);
    if code != HV_SUCCESS {
        let error = HvError::from_code(code);
        tracing::error!(target: "ternvale::hv", error = %error, "hv_vcpu_destroy failed");
        return Err(error);
    }
    Ok(())
}

/// Read a general register, PC, or CPSR.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all, fields(vcpu_id = id, reg = ?reg))]
pub fn get_reg(id: u64, reg: Reg) -> Result<u64, HvError> {
    let code_reg = reg.code()?;
    let mut value = 0u64;
    // SAFETY: `value` is a writable local. `id` is a live vCPU on this thread.
    let raw = unsafe { hv_vcpu_get_reg(id, code_reg, &mut value) };
    let code =
        ternvale_log::log_hv_call!("hv_vcpu_get_reg", format!("id={id:#x} reg={code_reg}"), raw);
    finish(code, value)
}

/// Write a general register, PC, or CPSR.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::hv",
    skip_all,
    fields(vcpu_id = id, reg = ?reg, value = format!("{:#x}", value))
)]
pub fn set_reg(id: u64, reg: Reg, value: u64) -> Result<(), HvError> {
    let code_reg = reg.code()?;
    // SAFETY: `id` is a live vCPU on this thread. `value` is copied by the kernel.
    let raw = unsafe { hv_vcpu_set_reg(id, code_reg, value) };
    let code = ternvale_log::log_hv_call!(
        "hv_vcpu_set_reg",
        format!("id={id:#x} reg={code_reg} value={value:#x}"),
        raw
    );
    finish(code, ())
}

/// Read a system register.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all, fields(vcpu_id = id, reg = ?reg))]
pub fn get_sys_reg(id: u64, reg: SysReg) -> Result<u64, HvError> {
    let code_reg = reg.code();
    let mut value = 0u64;
    // SAFETY: `value` is a writable local. `id` is a live vCPU on this thread.
    let raw = unsafe { hv_vcpu_get_sys_reg(id, code_reg, &mut value) };
    let code = ternvale_log::log_hv_call!(
        "hv_vcpu_get_sys_reg",
        format!("id={id:#x} reg={code_reg:#x}"),
        raw
    );
    finish(code, value)
}

/// Write a system register.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::hv",
    skip_all,
    fields(vcpu_id = id, reg = ?reg, value = format!("{:#x}", value))
)]
pub fn set_sys_reg(id: u64, reg: SysReg, value: u64) -> Result<(), HvError> {
    let code_reg = reg.code();
    // SAFETY: `id` is a live vCPU on this thread. `value` is copied by the kernel.
    let raw = unsafe { hv_vcpu_set_sys_reg(id, code_reg, value) };
    let code = ternvale_log::log_hv_call!(
        "hv_vcpu_set_sys_reg",
        format!("id={id:#x} reg={code_reg:#x} value={value:#x}"),
        raw
    );
    finish(code, ())
}

/// Run until the next exit. The kernel writes [`VcpuExit`] before returning.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all, fields(vcpu_id = id))]
pub fn vcpu_run(id: u64) -> Result<(), HvError> {
    // SAFETY: `id` is a live vCPU on this thread. The exit object stays valid.
    let raw = unsafe { hv_vcpu_run(id) };
    let code = ternvale_log::log_hv_call!("hv_vcpu_run", format!("id={id:#x}"), raw);
    finish(code, ())
}

/// Ask the kernel to cancel `ids`. Safe to call from a thread that does not own them.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all, fields(count = ids.len()))]
pub fn vcpus_exit(ids: &[u64]) -> Result<(), HvError> {
    let count = u32::try_from(ids.len()).map_err(|_| {
        tracing::error!(target: "ternvale::hv", count = ids.len(), "too many vcpus to exit");
        HvError::BadArgument {
            code: crate::HV_BAD_ARGUMENT,
        }
    })?;
    // SAFETY: `ids` is a live slice of vCPU ids for this process's VM.
    // `hv_vcpus_exit` only reads the list for the duration of the call.
    let raw = unsafe { hv_vcpus_exit(ids.as_ptr(), count) };
    let code = ternvale_log::log_hv_call!("hv_vcpus_exit", format!("count={count}"), raw);
    finish(code, ())
}

fn finish<T>(code: i32, value: T) -> Result<T, HvError> {
    if code != HV_SUCCESS {
        let error = HvError::from_code(code);
        tracing::error!(target: "ternvale::hv", error = %error, "hv vcpu call failed");
        return Err(error);
    }
    Ok(value)
}
