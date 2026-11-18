//! ICH / ICC_SRE_EL2 when the VM was created with EL2 enabled.
//!
//! Encodings are `hv_gic_ich_reg_t` / `HV_GIC_ICC_REG_SRE_EL2` in
//! `hv_gic_types.h` (macos(15.0)). `hv_gic.h` requires the vCPU owner thread.

use crate::error::HvError;
use crate::gic::{check, gic_fn, Gic};

impl Gic {
    /// Read an ICH register by `hv_gic_ich_reg_t` encoding. Owner thread only.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gic",
        skip_all,
        fields(vcpu_id = vcpu, enc = format!("{:#x}", enc))
    )]
    pub fn get_ich_enc(&self, vcpu: u64, enc: u16) -> Result<u64, HvError> {
        let func =
            gic_fn::<unsafe extern "C" fn(u64, u16, *mut u64) -> i32>(c"hv_gic_get_ich_reg")?;
        let mut value = 0u64;
        // SAFETY: `value` is writable. `vcpu` is a live id on this thread. EL2
        // was enabled at `hv_vm_create` when nested_virt is on.
        let raw = unsafe { func(vcpu, enc, &mut value) };
        let code = ternvale_log::log_hv_call!(
            "hv_gic_get_ich_reg",
            format!("vcpu={vcpu:#x} reg={enc:#x}"),
            raw
        );
        check(code, "hv_gic_get_ich_reg")?;
        Ok(value)
    }

    /// Write an ICH register by `hv_gic_ich_reg_t` encoding. Owner thread only.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gic",
        skip_all,
        fields(vcpu_id = vcpu, enc = format!("{:#x}", enc), value = format!("{:#x}", value))
    )]
    pub fn set_ich_enc(&self, vcpu: u64, enc: u16, value: u64) -> Result<(), HvError> {
        let func = gic_fn::<unsafe extern "C" fn(u64, u16, u64) -> i32>(c"hv_gic_set_ich_reg")?;
        // SAFETY: `vcpu` is a live id on this thread.
        let raw = unsafe { func(vcpu, enc, value) };
        let code = ternvale_log::log_hv_call!(
            "hv_gic_set_ich_reg",
            format!("vcpu={vcpu:#x} reg={enc:#x} value={value:#x}"),
            raw
        );
        check(code, "hv_gic_set_ich_reg")
    }

    /// Read an ICC register by `hv_gic_icc_reg_t` encoding (includes `SRE_EL2`).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gic",
        skip_all,
        fields(vcpu_id = vcpu, enc = format!("{:#x}", enc))
    )]
    pub fn get_icc_enc(&self, vcpu: u64, enc: u16) -> Result<u64, HvError> {
        let func =
            gic_fn::<unsafe extern "C" fn(u64, u16, *mut u64) -> i32>(c"hv_gic_get_icc_reg")?;
        let mut value = 0u64;
        // SAFETY: `value` is writable. `vcpu` is a live id on this thread.
        let raw = unsafe { func(vcpu, enc, &mut value) };
        let code = ternvale_log::log_hv_call!(
            "hv_gic_get_icc_reg",
            format!("vcpu={vcpu:#x} reg={enc:#x}"),
            raw
        );
        check(code, "hv_gic_get_icc_reg")?;
        Ok(value)
    }

    /// Write an ICC register by `hv_gic_icc_reg_t` encoding.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gic",
        skip_all,
        fields(vcpu_id = vcpu, enc = format!("{:#x}", enc), value = format!("{:#x}", value))
    )]
    pub fn set_icc_enc(&self, vcpu: u64, enc: u16, value: u64) -> Result<(), HvError> {
        let func = gic_fn::<unsafe extern "C" fn(u64, u16, u64) -> i32>(c"hv_gic_set_icc_reg")?;
        // SAFETY: `vcpu` is a live id on this thread.
        let raw = unsafe { func(vcpu, enc, value) };
        let code = ternvale_log::log_hv_call!(
            "hv_gic_set_icc_reg",
            format!("vcpu={vcpu:#x} reg={enc:#x} value={value:#x}"),
            raw
        );
        check(code, "hv_gic_set_icc_reg")
    }
}
