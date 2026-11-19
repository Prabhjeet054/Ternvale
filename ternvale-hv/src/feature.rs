//! Host ID / feature registers via `hv_vcpu_config_get_feature_reg`.
//!
//! `hv_vcpu_config.h` (macos(11.0)) has **get** only. There is no
//! `hv_vcpu_config_set_feature_reg`. TODO(verify): whether every `HV_SYS_REG_ID_*`
//! write through `hv_vcpu_set_sys_reg` is honoured on every chip.

use std::ffi::c_void;

use crate::el2::dlsym_fn;
use crate::error::{HvError, HV_SUCCESS};
use crate::ffi;

/// `hv_feature_reg_t` in `hv_vcpu_config.h`. Sequential from 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FeatureReg {
    /// `HV_FEATURE_REG_ID_AA64DFR0_EL1`.
    IdAa64Dfr0El1 = 0,
    /// `HV_FEATURE_REG_ID_AA64DFR1_EL1`.
    IdAa64Dfr1El1 = 1,
    /// `HV_FEATURE_REG_ID_AA64ISAR0_EL1`.
    IdAa64Isar0El1 = 2,
    /// `HV_FEATURE_REG_ID_AA64ISAR1_EL1`.
    IdAa64Isar1El1 = 3,
    /// `HV_FEATURE_REG_ID_AA64MMFR0_EL1`.
    IdAa64Mmfr0El1 = 4,
    /// `HV_FEATURE_REG_ID_AA64MMFR1_EL1`.
    IdAa64Mmfr1El1 = 5,
    /// `HV_FEATURE_REG_ID_AA64MMFR2_EL1`.
    IdAa64Mmfr2El1 = 6,
    /// `HV_FEATURE_REG_ID_AA64PFR0_EL1`.
    IdAa64Pfr0El1 = 7,
    /// `HV_FEATURE_REG_ID_AA64PFR1_EL1`.
    IdAa64Pfr1El1 = 8,
    /// `HV_FEATURE_REG_CTR_EL0`.
    CtrEl0 = 9,
    /// `HV_FEATURE_REG_CLIDR_EL1`.
    ClidrEl1 = 10,
    /// `HV_FEATURE_REG_DCZID_EL0`.
    DczidEl0 = 11,
    /// `HV_FEATURE_REG_ID_AA64SMFR0_EL1` (macos(15.2)).
    IdAa64Smfr0El1 = 12,
    /// `HV_FEATURE_REG_ID_AA64ZFR0_EL1` (macos(15.2)).
    IdAa64Zfr0El1 = 13,
    /// `HV_FEATURE_REG_ID_AA64ISAR2_EL1` (macos(27.0)).
    IdAa64Isar2El1 = 14,
    /// `HV_FEATURE_REG_ID_AA64PFR2_EL1` (macos(27.0)).
    IdAa64Pfr2El1 = 15,
    /// `HV_FEATURE_REG_ID_AA64MMFR3_EL1` (macos(27.0)).
    IdAa64Mmfr3El1 = 16,
    /// `HV_FEATURE_REG_ID_AA64MMFR4_EL1` (macos(27.0)).
    IdAa64Mmfr4El1 = 17,
}

impl FeatureReg {
    /// ARM / SDK name used in logs, snapshots, and mismatch lists.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::IdAa64Dfr0El1 => "ID_AA64DFR0_EL1",
            Self::IdAa64Dfr1El1 => "ID_AA64DFR1_EL1",
            Self::IdAa64Isar0El1 => "ID_AA64ISAR0_EL1",
            Self::IdAa64Isar1El1 => "ID_AA64ISAR1_EL1",
            Self::IdAa64Mmfr0El1 => "ID_AA64MMFR0_EL1",
            Self::IdAa64Mmfr1El1 => "ID_AA64MMFR1_EL1",
            Self::IdAa64Mmfr2El1 => "ID_AA64MMFR2_EL1",
            Self::IdAa64Pfr0El1 => "ID_AA64PFR0_EL1",
            Self::IdAa64Pfr1El1 => "ID_AA64PFR1_EL1",
            Self::CtrEl0 => "CTR_EL0",
            Self::ClidrEl1 => "CLIDR_EL1",
            Self::DczidEl0 => "DCZID_EL0",
            Self::IdAa64Smfr0El1 => "ID_AA64SMFR0_EL1",
            Self::IdAa64Zfr0El1 => "ID_AA64ZFR0_EL1",
            Self::IdAa64Isar2El1 => "ID_AA64ISAR2_EL1",
            Self::IdAa64Pfr2El1 => "ID_AA64PFR2_EL1",
            Self::IdAa64Mmfr3El1 => "ID_AA64MMFR3_EL1",
            Self::IdAa64Mmfr4El1 => "ID_AA64MMFR4_EL1",
        }
    }

    /// `hv_sys_reg_t` for ID registers that exist in `hv_vcpu_types.h`.
    #[must_use]
    pub fn sys_reg_encoding(self) -> Option<u16> {
        Some(match self {
            Self::IdAa64Pfr0El1 => 0xc020,
            Self::IdAa64Pfr1El1 => 0xc021,
            Self::IdAa64Pfr2El1 => 0xc022,
            Self::IdAa64Zfr0El1 => 0xc024,
            Self::IdAa64Smfr0El1 => 0xc025,
            Self::IdAa64Dfr0El1 => 0xc028,
            Self::IdAa64Dfr1El1 => 0xc029,
            Self::IdAa64Isar0El1 => 0xc030,
            Self::IdAa64Isar1El1 => 0xc031,
            Self::IdAa64Isar2El1 => 0xc032,
            Self::IdAa64Mmfr0El1 => 0xc038,
            Self::IdAa64Mmfr1El1 => 0xc039,
            Self::IdAa64Mmfr2El1 => 0xc03a,
            Self::IdAa64Mmfr3El1 => 0xc03b,
            Self::IdAa64Mmfr4El1 => 0xc03c,
            Self::CtrEl0 | Self::ClidrEl1 | Self::DczidEl0 => return None,
        })
    }

    const ALL: [Self; 18] = [
        Self::IdAa64Dfr0El1,
        Self::IdAa64Dfr1El1,
        Self::IdAa64Isar0El1,
        Self::IdAa64Isar1El1,
        Self::IdAa64Mmfr0El1,
        Self::IdAa64Mmfr1El1,
        Self::IdAa64Mmfr2El1,
        Self::IdAa64Pfr0El1,
        Self::IdAa64Pfr1El1,
        Self::CtrEl0,
        Self::ClidrEl1,
        Self::DczidEl0,
        Self::IdAa64Smfr0El1,
        Self::IdAa64Zfr0El1,
        Self::IdAa64Isar2El1,
        Self::IdAa64Pfr2El1,
        Self::IdAa64Mmfr3El1,
        Self::IdAa64Mmfr4El1,
    ];
}

/// One `hv_vcpu_config_get_feature_reg` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureValue {
    /// Register selector.
    pub reg: FeatureReg,
    /// Value, or `None` when the host rejected this selector.
    pub value: Option<u64>,
}

/// Read every `hv_feature_reg_t` from a fresh vCPU config (no VM).
#[tracing::instrument(level = "debug", target = "ternvale::cpuprofile", skip_all)]
pub fn read_host_features() -> Result<Vec<FeatureValue>, HvError> {
    let create = dlsym_fn::<unsafe extern "C" fn() -> *mut c_void>(c"hv_vcpu_config_create")
        .ok_or_else(|| {
            tracing::error!(
                target: "ternvale::cpuprofile",
                "hv_vcpu_config_create missing"
            );
            HvError::Unsupported {
                code: crate::error::HV_UNSUPPORTED,
            }
        })?;
    let get = dlsym_fn::<unsafe extern "C" fn(*mut c_void, u32, *mut u64) -> i32>(
        c"hv_vcpu_config_get_feature_reg",
    )
    .ok_or_else(|| {
        tracing::error!(
            target: "ternvale::cpuprofile",
            "hv_vcpu_config_get_feature_reg missing"
        );
        HvError::Unsupported {
            code: crate::error::HV_UNSUPPORTED,
        }
    })?;
    // SAFETY: the framework returns an os_object or NULL.
    let config = unsafe { create() };
    if config.is_null() {
        tracing::error!(target: "ternvale::cpuprofile", "hv_vcpu_config_create returned null");
        return Err(HvError::VmConfigNull);
    }
    let mut out = Vec::new();
    for reg in FeatureReg::ALL {
        let mut value = 0u64;
        // SAFETY: `config` is live; `value` is writable.
        let raw = unsafe { get(config, reg as u32, &mut value) };
        let code = ternvale_log::log_hv_call!(
            "hv_vcpu_config_get_feature_reg",
            format!("reg={} ({})", reg.name(), reg as u32),
            raw
        );
        if code == HV_SUCCESS {
            tracing::debug!(
                target: "ternvale::cpuprofile",
                reg = reg.name(),
                value = format!("{value:#x}"),
                "host feature register"
            );
            out.push(FeatureValue {
                reg,
                value: Some(value),
            });
        } else {
            tracing::debug!(
                target: "ternvale::cpuprofile",
                reg = reg.name(),
                code,
                "host feature register not available"
            );
            out.push(FeatureValue { reg, value: None });
        }
    }
    // SAFETY: `config` came from `hv_vcpu_config_create`.
    unsafe { ffi::os_release(config) };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::FeatureReg;

    #[test]
    fn sdk_enum_order_and_id_encodings() {
        assert_eq!(FeatureReg::IdAa64Dfr0El1 as u32, 0);
        assert_eq!(FeatureReg::DczidEl0 as u32, 11);
        assert_eq!(FeatureReg::IdAa64Mmfr4El1 as u32, 17);
        assert_eq!(FeatureReg::IdAa64Dfr0El1.sys_reg_encoding(), Some(0xc028));
        assert_eq!(FeatureReg::CtrEl0.sys_reg_encoding(), None);
        assert_eq!(FeatureReg::IdAa64Pfr0El1.name(), "ID_AA64PFR0_EL1");
    }
}
