//! In-kernel GICv3. Create it after `hv_vm_create` and before any vCPU.
//!
//! Distributor and redistributor guest addresses come from the caller. The
//! device tree uses the same `GIC_DIST_*` and `GIC_REDIST_*` constants in
//! `ternvale-vmm` `platform.rs`.

use std::ffi::{c_void, CStr};

use crate::error::{HvError, HV_SUCCESS};
use crate::ffi::{self, os_release};
use crate::gic_order::lock_order;
use crate::osver::ensure_gic_os;

/// Called after a successful `hv_vm_create`.
#[tracing::instrument(level = "debug", target = "ternvale::gic", skip_all)]
pub(crate) fn note_vm_created() {
    lock_order().on_vm_created();
}

/// Called when the VM is destroyed.
#[tracing::instrument(level = "debug", target = "ternvale::gic", skip_all)]
pub(crate) fn note_vm_destroyed() {
    lock_order().on_vm_destroyed();
}

/// Reject `hv_vcpu_create` until [`Vm::create_gic`](crate::Vm::create_gic) has run.
#[tracing::instrument(level = "debug", target = "ternvale::gic", skip_all)]
pub(crate) fn note_vcpu_create() -> Result<(), HvError> {
    lock_order().begin_vcpu().inspect_err(|error| {
        tracing::error!(target: "ternvale::gic", error = %error, "rejected vCPU create before GIC");
    })
}

/// ICC system registers used to drive the CPU interface (`hv_gic_types.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IccReg {
    /// `HV_GIC_ICC_REG_PMR_EL1` (`0xc230`).
    PmrEl1,
    /// `HV_GIC_ICC_REG_CTLR_EL1` (`0xc664`).
    CtlrEl1,
    /// `HV_GIC_ICC_REG_SRE_EL1` (`0xc665`).
    SreEl1,
    /// `HV_GIC_ICC_REG_IGRPEN1_EL1` (`0xc667`).
    Igrpen1El1,
}

impl IccReg {
    fn code(self) -> u16 {
        match self {
            Self::PmrEl1 => 0xc230,
            Self::CtlrEl1 => 0xc664,
            Self::SreEl1 => 0xc665,
            Self::Igrpen1El1 => 0xc667,
        }
    }
}

/// The process-wide in-kernel GICv3. Drop does not destroy it; the VM does.
#[derive(Debug)]
pub struct Gic {
    distributor: u64,
    redistributor: u64,
}

impl Gic {
    /// Raise or lower SPI `irq`. `level` true also pulses an edge-triggered line.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gic",
        skip_all,
        fields(irq, level)
    )]
    pub fn set_spi(&self, irq: u32, level: bool) -> Result<(), HvError> {
        tracing::Span::current().record("irq", irq);
        tracing::Span::current().record("level", level);
        let func = gic_fn::<unsafe extern "C" fn(u32, bool) -> i32>(c"hv_gic_set_spi")?;
        // SAFETY: the GIC exists for this VM. `irq` is passed through; the kernel
        // returns HV_BAD_ARGUMENT when it is outside the SPI range.
        let raw = unsafe { func(irq, level) };
        let code =
            ternvale_log::log_hv_call!("hv_gic_set_spi", format!("intid={irq} level={level}"), raw);
        check(code, "hv_gic_set_spi")
    }

    /// Read an ICC system register. Must run on the vCPU's thread.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gic",
        skip_all,
        fields(vcpu_id = vcpu, reg = ?reg)
    )]
    pub fn get_icc(&self, vcpu: u64, reg: IccReg) -> Result<u64, HvError> {
        let func =
            gic_fn::<unsafe extern "C" fn(u64, u16, *mut u64) -> i32>(c"hv_gic_get_icc_reg")?;
        let mut value = 0u64;
        let code_reg = reg.code();
        // SAFETY: `value` is a writable local. `vcpu` is a kernel id. The header
        // requires the owning thread; the caller keeps that.
        let raw = unsafe { func(vcpu, code_reg, &mut value) };
        let code = ternvale_log::log_hv_call!(
            "hv_gic_get_icc_reg",
            format!("vcpu={vcpu:#x} reg={code_reg:#x}"),
            raw
        );
        check(code, "hv_gic_get_icc_reg")?;
        Ok(value)
    }

    /// Write an ICC system register. Must run on the vCPU's thread.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gic",
        skip_all,
        fields(vcpu_id = vcpu, reg = ?reg, value = format!("{:#x}", value))
    )]
    pub fn set_icc(&self, vcpu: u64, reg: IccReg, value: u64) -> Result<(), HvError> {
        let func = gic_fn::<unsafe extern "C" fn(u64, u16, u64) -> i32>(c"hv_gic_set_icc_reg")?;
        let code_reg = reg.code();
        // SAFETY: `vcpu` is a kernel id. The header requires the owning thread.
        let raw = unsafe { func(vcpu, code_reg, value) };
        let code = ternvale_log::log_hv_call!(
            "hv_gic_set_icc_reg",
            format!("vcpu={vcpu:#x} reg={code_reg:#x} value={value:#x}"),
            raw
        );
        check(code, "hv_gic_set_icc_reg")
    }

    /// Opaque GIC state, excluding CPU registers (`hv_gic_state.h`).
    #[tracing::instrument(level = "debug", target = "ternvale::gic", skip_all)]
    pub fn capture_state(&self) -> Result<Vec<u8>, HvError> {
        let create = gic_fn::<unsafe extern "C" fn() -> *mut c_void>(c"hv_gic_state_create")?;
        // SAFETY: the VM is stopped by the caller, as hv_gic_state.h requires.
        // A null result means the kernel cannot represent the state.
        let state = unsafe { create() };
        ternvale_log::log_hv_call!("hv_gic_state_create", "none", i32::from(state.is_null()));
        if state.is_null() {
            tracing::error!(target: "ternvale::gic", "hv_gic_state_create returned null");
            return Err(HvError::GicNull {
                what: "hv_gic_state_t",
            });
        }
        let captured = (|| {
            let mut size = 0usize;
            let get_size = gic_fn::<unsafe extern "C" fn(*mut c_void, *mut usize) -> i32>(
                c"hv_gic_state_get_size",
            )?;
            // SAFETY: `state` is the object just created. `size` is writable.
            let raw = unsafe { get_size(state, &mut size) };
            let code =
                ternvale_log::log_hv_call!("hv_gic_state_get_size", format!("size={size}"), raw);
            check(code, "hv_gic_state_get_size")?;
            let mut buf = vec![0u8; size];
            let get_data = gic_fn::<unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32>(
                c"hv_gic_state_get_data",
            )?;
            // SAFETY: `buf` has `size` bytes, which the framework asked for.
            let raw = unsafe { get_data(state, buf.as_mut_ptr().cast()) };
            let code =
                ternvale_log::log_hv_call!("hv_gic_state_get_data", format!("size={size}"), raw);
            check(code, "hv_gic_state_get_data")?;
            tracing::info!(target: "ternvale::gic", bytes = size, "captured GIC state");
            Ok(buf)
        })();
        // SAFETY: `state` came from hv_gic_state_create and is released once.
        unsafe { os_release(state) };
        captured
    }

    /// Restore distributor state. CPU registers are restored separately.
    #[tracing::instrument(level = "debug", target = "ternvale::gic", skip(data), fields(bytes = data.len()))]
    pub fn restore_state(&self, data: &[u8]) -> Result<(), HvError> {
        let func =
            gic_fn::<unsafe extern "C" fn(*const c_void, usize) -> i32>(c"hv_gic_set_state")?;
        // SAFETY: `data` is a readable buffer of `data.len()` bytes. The header
        // requires vCPUs to exist and not to have run yet.
        let raw = unsafe { func(data.as_ptr().cast(), data.len()) };
        let code =
            ternvale_log::log_hv_call!("hv_gic_set_state", format!("size={}", data.len()), raw);
        check(code, "hv_gic_set_state")
    }

    /// Distributor GPA passed to `hv_gic_config_set_distributor_base`.
    #[tracing::instrument(level = "debug", target = "ternvale::gic", skip_all)]
    pub fn distributor_base(&self) -> u64 {
        self.distributor
    }

    /// Redistributor GPA passed to `hv_gic_config_set_redistributor_base`.
    #[tracing::instrument(level = "debug", target = "ternvale::gic", skip_all)]
    pub fn redistributor_base(&self) -> u64 {
        self.redistributor
    }
}

/// Create the GICv3 after the VM exists and before any vCPU.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::gic",
    skip_all,
    fields(
        distributor = format!("{:#x}", distributor),
        redistributor = format!("{:#x}", redistributor)
    )
)]
pub(crate) fn create_gic(distributor: u64, redistributor: u64) -> Result<Gic, HvError> {
    ensure_gic_os()?;
    let mut order = lock_order();
    order.begin_gic().inspect_err(|error| {
        tracing::error!(
            target: "ternvale::gic",
            error = %error,
            "rejected GIC create; required order is hv_vm_create, then GIC, then vCPUs"
        );
    })?;
    tracing::info!(
        target: "ternvale::gic",
        distributor = format!("{:#x}", distributor),
        redistributor = format!("{:#x}", redistributor),
        "creating GIC after hv_vm_create and before vCPUs"
    );
    let created = install(distributor, redistributor);
    if created.is_err() {
        order.rollback_gic();
    }
    created
}

fn install(distributor: u64, redistributor: u64) -> Result<Gic, HvError> {
    let dist_size = query_usize(c"hv_gic_get_distributor_size")?;
    let redist_size = query_usize(c"hv_gic_get_redistributor_region_size")?;
    let dist_align = query_usize(c"hv_gic_get_distributor_base_alignment")?;
    let redist_align = query_usize(c"hv_gic_get_redistributor_base_alignment")?;
    tracing::info!(
        target: "ternvale::gic",
        distributor_size = format!("{:#x}", dist_size),
        redistributor_region_size = format!("{:#x}", redist_size),
        distributor_align = format!("{:#x}", dist_align),
        redistributor_align = format!("{:#x}", redist_align),
        "framework GIC sizes"
    );
    aligned(distributor, dist_align, "distributor")?;
    aligned(redistributor, redist_align, "redistributor")?;

    let config_create = gic_fn::<unsafe extern "C" fn() -> *mut c_void>(c"hv_gic_config_create")?;
    // SAFETY: hv_gic_config.h says to create the config after the VM. The
    // object is retained and released below, including on failure.
    let config = unsafe { config_create() };
    ternvale_log::log_hv_call!("hv_gic_config_create", "none", i32::from(config.is_null()));
    if config.is_null() {
        tracing::error!(target: "ternvale::gic", "hv_gic_config_create returned null");
        return Err(HvError::GicNull {
            what: "hv_gic_config_t",
        });
    }
    let installed = (|| {
        call_base(c"hv_gic_config_set_distributor_base", config, distributor)?;
        call_base(
            c"hv_gic_config_set_redistributor_base",
            config,
            redistributor,
        )?;
        let create = gic_fn::<unsafe extern "C" fn(*mut c_void) -> i32>(c"hv_gic_create")?;
        // SAFETY: `config` is the live config object. No vCPU has been created.
        let raw = unsafe { create(config) };
        let code = ternvale_log::log_hv_call!(
            "hv_gic_create",
            format!("distributor={distributor:#x} redistributor={redistributor:#x}"),
            raw
        );
        check(code, "hv_gic_create")?;
        tracing::info!(
            target: "ternvale::gic",
            distributor = format!("{:#x}", distributor),
            redistributor = format!("{:#x}", redistributor),
            "GIC created"
        );
        Ok(Gic {
            distributor,
            redistributor,
        })
    })();
    // SAFETY: `config` came from hv_gic_config_create and is released once.
    unsafe { os_release(config) };
    installed
}

fn aligned(base: u64, alignment: usize, region: &'static str) -> Result<(), HvError> {
    if alignment == 0 || base % (alignment as u64) == 0 {
        return Ok(());
    }
    tracing::warn!(
        target: "ternvale::gic",
        region,
        base = format!("{:#x}", base),
        alignment = format!("{:#x}", alignment),
        "rejected misaligned GIC base"
    );
    Err(HvError::GicMisaligned {
        region,
        base,
        alignment,
    })
}

fn call_base(name: &CStr, config: *mut c_void, base: u64) -> Result<(), HvError> {
    let func = gic_fn::<unsafe extern "C" fn(*mut c_void, u64) -> i32>(name)?;
    // SAFETY: `config` is the object from hv_gic_config_create, not yet released.
    let raw = unsafe { func(config, base) };
    let label = name_str(name);
    let code = ternvale_log::log_hv_call!(label, format!("base={base:#x}"), raw);
    check(code, label)
}

fn query_usize(name: &CStr) -> Result<usize, HvError> {
    let func = gic_fn::<unsafe extern "C" fn(*mut usize) -> i32>(name)?;
    let mut value = 0usize;
    // SAFETY: `value` is a writable local. These parameter calls do not need a VM.
    let raw = unsafe { func(&mut value) };
    let label = name_str(name);
    let code = ternvale_log::log_hv_call!(label, format!("value={value:#x}"), raw);
    check(code, label)?;
    Ok(value)
}

fn gic_fn<T>(name: &CStr) -> Result<T, HvError> {
    // RTLD_DEFAULT is `((void *) -2)` in `<dlfcn.h>`.
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
    // SAFETY: `name` is a NUL-terminated literal. `dlsym` returns the process-wide
    // symbol or NULL. A non-null result is the Hypervisor.framework function `T`.
    let symbol = unsafe { ffi::dlsym(RTLD_DEFAULT, name.as_ptr()) };
    if symbol.is_null() {
        tracing::error!(target: "ternvale::gic", name = name_str(name), "GICv3 symbol is missing");
        return Err(HvError::GicUnavailable);
    }
    Ok(unsafe { std::mem::transmute_copy(&symbol) })
}

fn name_str(name: &CStr) -> &str {
    name.to_str().unwrap_or("hv_gic")
}

fn check(code: i32, what: &str) -> Result<(), HvError> {
    if code == HV_SUCCESS {
        return Ok(());
    }
    let error = HvError::from_code(code);
    tracing::error!(target: "ternvale::gic", what, error = %error, "GIC call failed");
    Err(error)
}
