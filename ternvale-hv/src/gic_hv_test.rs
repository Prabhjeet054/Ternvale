//! Hypervisor tests for GICv3 create order and the macOS version gate.

use super::{vcpu_create, vcpu_destroy, HvError, Vm};
use crate::osver::set_gic_os_version_override;

const DIST: u64 = 0x0800_0000;
const REDIST: u64 = 0x080a_0000;

struct VersionGuard;

impl Drop for VersionGuard {
    fn drop(&mut self) {
        set_gic_os_version_override(None);
    }
}

#[test]
#[ignore = "needs-hv"]
fn injected_old_macos_rejects_gic_create() {
    let _guard = VersionGuard;
    set_gic_os_version_override(Some((14, 0)));
    let vm = Vm::create().expect("vm");
    let error = vm.create_gic(DIST, REDIST).expect_err("macos 14");
    assert!(
        matches!(
            error,
            HvError::GicOsUnsupported {
                major: 14,
                minor: 0
            }
        ),
        "{error}"
    );
    assert!(error.to_string().contains("15.0"), "{error}");
    drop(vm);
}

#[test]
#[ignore = "needs-hv"]
fn creates_gic_before_one_vcpu_and_rejects_bad_order() {
    let vm = Vm::create().expect("vm");
    let early = vcpu_create().expect_err("vcpu before gic");
    assert!(matches!(early, HvError::VcpuBeforeGic), "{early}");

    let gic = vm.create_gic(DIST, REDIST).expect("gic");
    let again = vm.create_gic(DIST, REDIST).expect_err("second gic");
    assert!(matches!(again, HvError::GicExists), "{again}");

    let (id, _exit) = vcpu_create().expect("vcpu");
    let late = vm.create_gic(DIST, REDIST).expect_err("gic after vcpu");
    assert!(matches!(late, HvError::GicAfterVcpu), "{late}");

    gic.set_spi(32, true).expect("spi");
    vcpu_destroy(id).expect("destroy vcpu");
    drop(gic);
    drop(vm);
}
