//! Safe wrappers around Apple's Hypervisor.framework.
//!
//! Raw FFI stays in this crate. Callers use [`Vm`], which owns the single
//! process-wide VM from `hv_vm_create` until `hv_vm_destroy`.

mod error;
mod ffi;
mod gic;
mod gic_order;
mod map;
mod osver;
mod vcpu;
mod vm;

pub use error::{
    HvError, HV_BAD_ARGUMENT, HV_BUSY, HV_DENIED, HV_ERROR, HV_EXISTS, HV_ILLEGAL_GUEST_STATE,
    HV_NO_DEVICE, HV_NO_RESOURCES, HV_SUCCESS, HV_UNSUPPORTED,
};
pub use gic::{Gic, IccReg};
pub use map::{
    map_memory, unmap_memory, HV_MEMORY_EXEC, HV_MEMORY_READ, HV_MEMORY_RWX, HV_MEMORY_WRITE,
};
pub use vcpu::{
    get_reg, get_sys_reg, set_reg, set_sys_reg, vcpu_create, vcpu_destroy, vcpu_run, vcpus_exit,
    Reg, SysReg, VcpuExit, HV_EXIT_REASON_CANCELED, HV_EXIT_REASON_EXCEPTION,
    HV_EXIT_REASON_UNKNOWN, HV_EXIT_REASON_VTIMER_ACTIVATED,
};
pub use vm::Vm;

#[cfg(test)]
mod tests {
    use super::{
        HvError, Vm, HV_BAD_ARGUMENT, HV_BUSY, HV_DENIED, HV_ERROR, HV_EXISTS,
        HV_ILLEGAL_GUEST_STATE, HV_NO_DEVICE, HV_NO_RESOURCES, HV_UNSUPPORTED,
    };

    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-hv");
    }

    #[test]
    fn maps_arm64_hv_return_codes() {
        let cases = [
            (HV_ERROR, "HV_ERROR"),
            (HV_BUSY, "HV_BUSY"),
            (HV_BAD_ARGUMENT, "HV_BAD_ARGUMENT"),
            (HV_ILLEGAL_GUEST_STATE, "HV_ILLEGAL_GUEST_STATE"),
            (HV_NO_RESOURCES, "HV_NO_RESOURCES"),
            (HV_NO_DEVICE, "HV_NO_DEVICE"),
            (HV_DENIED, "HV_DENIED"),
            (HV_EXISTS, "HV_EXISTS"),
            (HV_UNSUPPORTED, "HV_UNSUPPORTED"),
        ];
        for (code, name) in cases {
            let error = HvError::from_code(code);
            assert!(error.to_string().contains(name), "{error}");
        }
        let unknown = HvError::from_code(0x1234);
        assert!(matches!(unknown, HvError::Unknown { code: 0x1234 }));
    }

    /// Placeholder so `make test-hv` has one signed binary to execute.
    #[test]
    #[ignore = "needs-hv"]
    fn hypervisor_entitlement_is_present() {
        assert!(true);
    }

    #[test]
    #[ignore = "needs-hv"]
    fn only_one_vm_per_process() {
        let dir = std::env::temp_dir().join(format!("ternvale-hv-{}-vm", std::process::id()));
        std::fs::create_dir_all(&dir).expect("log dir");
        let previous = std::env::var("TERNVALE_LOG").ok();
        // SAFETY: this ignored test is the only ternvale-hv test that reads
        // TERNVALE_LOG, and it restores the previous value before returning.
        unsafe { std::env::set_var("TERNVALE_LOG", "trace") };
        let mut config = ternvale_log::LogConfig::new("hvvm", dir.clone());
        config.level = "trace".to_string();
        let guard = ternvale_log::init(config).expect("init log");

        let first = Vm::create().expect("hv_vm_create");
        let second = std::thread::spawn(|| match Vm::create() {
            Ok(vm) => {
                drop(vm);
                String::from("second VM was created")
            }
            Err(error) => error.to_string(),
        })
        .join()
        .expect("second create thread");
        assert!(second.contains("already exists"), "{second}");
        drop(first);
        let again = Vm::create().expect("hv_vm_create after destroy");
        drop(again);

        let path = guard.log_path().to_path_buf();
        drop(guard);
        let text = std::fs::read_to_string(&path).expect("read hv log");
        let creates: Vec<_> = text
            .lines()
            .filter(|line| line.contains("hv_vm_create") && line.contains("result_code="))
            .collect();
        assert_eq!(creates.len(), 2, "{text}");
        assert!(
            creates.iter().all(|line| line.contains("result_code=0")),
            "{text}"
        );
        let sample = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/ternvale-log-samples/hv-vm-create.log");
        if let Some(parent) = sample.parent() {
            std::fs::create_dir_all(parent).expect("sample dir");
        }
        std::fs::write(&sample, &text).expect("sample log");
        let removed = std::fs::remove_dir_all(&dir);
        if let Err(err) = removed {
            panic!("remove {}: {err}", dir.display());
        }
        // SAFETY: same as the set above; no other test in this binary is in the filter.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("TERNVALE_LOG", value),
                None => std::env::remove_var("TERNVALE_LOG"),
            }
        }
    }
}
