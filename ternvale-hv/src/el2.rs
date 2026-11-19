//! Hypervisor.framework EL2 (nested virt): probe and `hv_vm_config_set_el2_enabled`.

use std::ffi::{c_void, CStr};

use crate::error::{HvError, HV_SUCCESS};
use crate::ffi;

/// `hv_vm_config_get_el2_supported` in `hv_vm_config.h`.
pub const GET_EL2_SUPPORTED: &CStr = c"hv_vm_config_get_el2_supported";
/// `hv_vm_config_get_el2_enabled` in `hv_vm_config.h`.
pub const GET_EL2_ENABLED: &CStr = c"hv_vm_config_get_el2_enabled";
/// `hv_vm_config_set_el2_enabled` in `hv_vm_config.h`.
pub const SET_EL2_ENABLED: &CStr = c"hv_vm_config_set_el2_enabled";
/// `hv_vm_config_create` (needed to read the default EL2-enabled flag).
const CONFIG_CREATE: &CStr = c"hv_vm_config_create";

/// Header `API_AVAILABLE(macos(15.0))` on the EL2 config calls.
pub const API_AVAILABLE_MACOS: &str = "15.0";

/// Outcome of [`probe`]. Never turns EL2 on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct El2Probe {
    /// Exact symbol from the SDK header.
    pub get_supported_symbol: &'static str,
    /// Exact setter symbol.
    pub set_enabled_symbol: &'static str,
    /// `API_AVAILABLE` from the header.
    pub api_available_macos: &'static str,
    /// `dlsym` found `hv_vm_config_get_el2_supported`.
    pub symbol_present: bool,
    /// `dlsym` found `hv_vm_config_set_el2_enabled`.
    pub set_symbol_present: bool,
    /// Result of `hv_vm_config_get_el2_supported`, if the call succeeded.
    pub supported: Option<bool>,
    /// Default `hv_vm_config_get_el2_enabled` on a fresh config (should be false).
    pub default_enabled: Option<bool>,
    /// Human-readable failure (missing symbol or non-zero `hv_return_t`).
    pub error: Option<String>,
}

/// True when `hv_vm_config_get_el2_supported` returned true.
#[tracing::instrument(level = "debug", target = "ternvale::nested", skip_all)]
pub fn host_supports_el2() -> bool {
    matches!(probe().supported, Some(true))
}

/// `hv_vm_config_create` then optionally `hv_vm_config_set_el2_enabled`.
/// Caller must `os_release` the pointer.
#[tracing::instrument(level = "debug", target = "ternvale::nested", skip_all, fields(el2))]
pub fn new_vm_config(el2: bool) -> Result<*mut c_void, HvError> {
    let create =
        dlsym_fn::<unsafe extern "C" fn() -> *mut c_void>(CONFIG_CREATE).ok_or_else(|| {
            tracing::error!(target: "ternvale::nested", "hv_vm_config_create missing");
            HvError::VmConfigNull
        })?;
    // SAFETY: framework returns an os_object or NULL.
    let config = unsafe { create() };
    if config.is_null() {
        tracing::error!(target: "ternvale::nested", "hv_vm_config_create returned null");
        return Err(HvError::VmConfigNull);
    }
    if el2 {
        if let Err(error) = set_el2_enabled(config, true) {
            // SAFETY: `config` is a live os_object from `hv_vm_config_create`.
            unsafe { crate::ffi::os_release(config) };
            return Err(error);
        }
    }
    Ok(config)
}

/// `hv_vm_config_set_el2_enabled`. macOS 15.0.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::nested",
    skip_all,
    fields(enabled)
)]
pub fn set_el2_enabled(config: *mut c_void, enabled: bool) -> Result<(), HvError> {
    let set = dlsym_fn::<unsafe extern "C" fn(*mut c_void, bool) -> i32>(SET_EL2_ENABLED)
        .ok_or(HvError::El2Unavailable)?;
    // SAFETY: `config` is a live `hv_vm_config_t`.
    let raw = unsafe { set(config, enabled) };
    let code = ternvale_log::log_hv_call!(
        "hv_vm_config_set_el2_enabled",
        format!("enabled={enabled}"),
        raw
    );
    if code != HV_SUCCESS {
        let error = HvError::from_code(code);
        tracing::error!(
            target: "ternvale::nested",
            error = %error,
            enabled,
            "hv_vm_config_set_el2_enabled failed"
        );
        return Err(error);
    }
    tracing::info!(
        target: "ternvale::nested",
        enabled,
        "Hypervisor.framework EL2 enabled on VM config"
    );
    Ok(())
}

/// Ask the host whether guests can run at EL2. Does not create a VM.
#[tracing::instrument(level = "info", target = "ternvale::nested", skip_all)]
pub fn probe() -> El2Probe {
    let mut out = El2Probe {
        get_supported_symbol: "hv_vm_config_get_el2_supported",
        set_enabled_symbol: "hv_vm_config_set_el2_enabled",
        api_available_macos: API_AVAILABLE_MACOS,
        symbol_present: false,
        set_symbol_present: false,
        supported: None,
        default_enabled: None,
        error: None,
    };
    out.set_symbol_present =
        dlsym_fn::<unsafe extern "C" fn(*mut c_void, bool) -> i32>(SET_EL2_ENABLED).is_some();
    tracing::debug!(
        target: "ternvale::nested",
        set_symbol = out.set_enabled_symbol,
        present = out.set_symbol_present,
        "hv_vm_config_set_el2_enabled looked up"
    );
    match dlsym_fn::<unsafe extern "C" fn(*mut bool) -> i32>(GET_EL2_SUPPORTED) {
        None => {
            tracing::warn!(
                target: "ternvale::nested",
                symbol = out.get_supported_symbol,
                "EL2 probe symbol missing (need macOS 15 Hypervisor.framework)"
            );
            out.error = Some("hv_vm_config_get_el2_supported not in Hypervisor.framework".into());
            return out;
        }
        Some(get_supported) => {
            out.symbol_present = true;
            let mut supported = false;
            // SAFETY: `supported` is a writable bool; the framework writes it on HV_SUCCESS.
            let raw = unsafe { get_supported(&mut supported) };
            let code = ternvale_log::log_hv_call!(
                "hv_vm_config_get_el2_supported",
                format!("supported={supported}"),
                raw
            );
            if code != HV_SUCCESS {
                let error = HvError::from_code(code);
                tracing::error!(
                    target: "ternvale::nested",
                    error = %error,
                    "hv_vm_config_get_el2_supported failed"
                );
                out.error = Some(error.to_string());
                return out;
            }
            out.supported = Some(supported);
            tracing::info!(
                target: "ternvale::nested",
                supported,
                "Hypervisor.framework EL2 supported flag"
            );
        }
    }
    out.default_enabled = probe_default_enabled();
    out
}

#[tracing::instrument(level = "debug", target = "ternvale::nested", skip_all)]
fn probe_default_enabled() -> Option<bool> {
    let create = dlsym_fn::<unsafe extern "C" fn() -> *mut c_void>(CONFIG_CREATE)?;
    let get_enabled =
        dlsym_fn::<unsafe extern "C" fn(*mut c_void, *mut bool) -> i32>(GET_EL2_ENABLED)?;
    // SAFETY: `hv_vm_config_create` returns an os_object or NULL. Released with `os_release`.
    let config = unsafe { create() };
    if config.is_null() {
        tracing::warn!(target: "ternvale::nested", "hv_vm_config_create returned null");
        return None;
    }
    let mut enabled = true;
    // SAFETY: `config` is a live hv_vm_config_t; `enabled` is writable.
    let raw = unsafe { get_enabled(config, &mut enabled) };
    unsafe { ffi::os_release(config) };
    let code = ternvale_log::log_hv_call!(
        "hv_vm_config_get_el2_enabled",
        format!("enabled={enabled}"),
        raw
    );
    if code != HV_SUCCESS {
        tracing::warn!(
            target: "ternvale::nested",
            code,
            "hv_vm_config_get_el2_enabled failed"
        );
        return None;
    }
    tracing::debug!(
        target: "ternvale::nested",
        enabled,
        "default EL2 enabled on a fresh hv_vm_config"
    );
    Some(enabled)
}

pub(crate) fn dlsym_fn<T>(name: &CStr) -> Option<T> {
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
    // SAFETY: `name` is a NUL-terminated literal. Non-null `dlsym` is `T`.
    let symbol = unsafe { ffi::dlsym(RTLD_DEFAULT, name.as_ptr()) };
    if symbol.is_null() {
        return None;
    }
    Some(unsafe { std::mem::transmute_copy(&symbol) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_symbol_names_match_the_header_spellings() {
        assert_eq!(
            GET_EL2_SUPPORTED.to_str().unwrap(),
            "hv_vm_config_get_el2_supported"
        );
        assert_eq!(
            GET_EL2_ENABLED.to_str().unwrap(),
            "hv_vm_config_get_el2_enabled"
        );
        assert_eq!(
            SET_EL2_ENABLED.to_str().unwrap(),
            "hv_vm_config_set_el2_enabled"
        );
        assert_eq!(API_AVAILABLE_MACOS, "15.0");
    }

    #[test]
    fn el2h_cpsr_is_el1h_with_m_bit_pattern_nine() {
        // EL1h + DAIF = 0x3c5 (M=0b0101). EL2h is M=0b1001 → 0x3c9.
        const CPSR_EL1H_MASKED: u64 = 0x3c5;
        const CPSR_EL2H_MASKED: u64 = 0x3c9;
        assert_eq!(CPSR_EL1H_MASKED ^ CPSR_EL2H_MASKED, 0b1100);
        assert_eq!(CPSR_EL2H_MASKED & 0xf, 0b1001);
    }
}
