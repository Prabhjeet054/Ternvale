//! Runs one `hvc #0` and checks the raw exception syndrome in the log.

use std::time::{Duration, Instant};

use super::{ExitReason, Vcpu, VcpuError};
use crate::{GuestMemory, GIC_DIST_BASE, GIC_REDIST_BASE, HOST_PAGE_SIZE};

/// AArch64 `hvc #0`.
const HVC0: u32 = 0xd400_0002;
const GUEST_PC: u64 = 0x4000_0000;
/// EL1h with DAIF masked. `M[3:0] = 0x5`, plus bits D, A, I, and F.
const CPSR_EL1H: u64 = 0x3c5;

#[test]
#[ignore = "needs-hv"]
fn runs_hvc0_and_rejects_run_from_another_thread() {
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-hvc", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let previous = std::env::var("TERNVALE_LOG").ok();
    // SAFETY: this ignored test is the only one that sets TERNVALE_LOG for the
    // hvc run, and it restores the previous value before returning.
    unsafe { std::env::set_var("TERNVALE_LOG", "trace") };
    let mut config = ternvale_log::LogConfig::new("hvc", dir.clone());
    config.level = "trace".to_string();
    let guard = ternvale_log::init(config).expect("log");

    let vm = ternvale_hv::Vm::create().expect("vm");
    vm.create_gic(GIC_DIST_BASE, GIC_REDIST_BASE).expect("gic");
    let mut memory = GuestMemory::new().expect("memory");
    memory.map(&vm, GUEST_PC, HOST_PAGE_SIZE).expect("map");
    memory.write_u32(GUEST_PC, HVC0).expect("store hvc");

    let vcpu = Vcpu::create(&vm).expect("vcpu");
    vcpu.set_cpsr(CPSR_EL1H).expect("cpsr");
    vcpu.set_pc(GUEST_PC).expect("pc");

    // The scoped thread must log to this test's subscriber; the process-wide
    // default may belong to an earlier test in the same binary.
    let dispatch = tracing::dispatcher::get_default(Clone::clone);
    let wrong = std::thread::scope(|scope| {
        scope
            .spawn(|| tracing::dispatcher::with_default(&dispatch, || vcpu.run()))
            .join()
            .expect("other thread")
    });
    let error = wrong.expect_err("other thread should fail");
    assert!(matches!(error, VcpuError::WrongThread { .. }), "{error:?}");
    assert!(error.to_string().contains("belongs to thread"), "{error}");

    let exit = vcpu.run().expect("run");
    let ExitReason::Exception { syndrome, .. } = exit else {
        panic!("expected an exception exit, got {exit:?}");
    };
    let syndrome_text = format!("{syndrome:#x}");

    drop(vcpu);
    drop(memory);
    drop(vm);
    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    assert!(
        text.contains("vcpu exception exit") && text.contains(&syndrome_text),
        "{text}"
    );
    assert!(
        text.contains("rejected vCPU call from a different thread"),
        "{text}"
    );
    let sample = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/ternvale-log-samples/hvc-exit.log");
    if let Some(parent) = sample.parent() {
        std::fs::create_dir_all(parent).expect("sample dir");
    }
    std::fs::write(&sample, &text).expect("sample");
    std::fs::remove_dir_all(&dir).expect("remove log dir");
    // SAFETY: same as the set above; this test restores the variable it changed.
    unsafe {
        match previous {
            Some(value) => std::env::set_var("TERNVALE_LOG", value),
            None => std::env::remove_var("TERNVALE_LOG"),
        }
    }
}

/// AArch64 `WFI`.
const WFI: u32 = 0xd503_207f;
/// EL1h with IRQs unmasked (`I` clear) so WFI is allowed to wait.
const CPSR_EL1H_IRQ_ON: u64 = 0x345;

#[test]
#[ignore = "needs-hv"]
fn wfi_parks_until_vtimer_deadline_not_a_fixed_slice() {
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-wfi", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let previous = std::env::var("TERNVALE_LOG").ok();
    // SAFETY: this ignored test sets TERNVALE_LOG and restores it.
    unsafe { std::env::set_var("TERNVALE_LOG", "info") };
    let mut config = ternvale_log::LogConfig::new("wfi", dir.clone());
    config.level = "info".to_string();
    let guard = ternvale_log::init(config).expect("log");

    let vm = ternvale_hv::Vm::create().expect("vm");
    vm.create_gic(GIC_DIST_BASE, GIC_REDIST_BASE).expect("gic");
    let mut memory = GuestMemory::new().expect("memory");
    memory.map(&vm, GUEST_PC, HOST_PAGE_SIZE).expect("map");
    memory.write_u32(GUEST_PC, WFI).expect("store wfi");

    let vcpu = Vcpu::create(&vm).expect("vcpu");
    vcpu.set_cpsr(CPSR_EL1H_IRQ_ON).expect("cpsr");
    vcpu.set_pc(GUEST_PC).expect("pc");

    let offset = ternvale_hv::get_vtimer_offset(vcpu.id).expect("offset");
    let now = ternvale_hv::host_ticks();
    let wait_ticks = 80_000_000; // ~80 ms on a 1:1 Apple Silicon timebase
    let cval = now.saturating_sub(offset).saturating_add(wait_ticks);
    vcpu.set_sys_reg(ternvale_hv::SysReg::CntvCvalEl0, cval)
        .expect("cval");
    vcpu.set_sys_reg(ternvale_hv::SysReg::CntvCtlEl0, 1)
        .expect("ctl enable");

    let started = Instant::now();
    let exit = vcpu.run().expect("run");
    let parked = started.elapsed();
    assert!(
        matches!(exit, ExitReason::Wfi | ExitReason::VtimerActivated),
        "idle wait should end as WFI or vtimer, got {exit:?}"
    );
    assert!(
        parked >= Duration::from_millis(40),
        "WFI must sleep until CVAL (~80ms), not a 10ms poll slice; parked {parked:?}"
    );
    assert!(
        parked < Duration::from_millis(500),
        "WFI must wake near CVAL, not hang; parked {parked:?}"
    );

    drop(vcpu);
    drop(memory);
    drop(vm);
    drop(guard);
    std::fs::remove_dir_all(&dir).expect("remove log dir");
    // SAFETY: restore TERNVALE_LOG.
    unsafe {
        match previous {
            Some(value) => std::env::set_var("TERNVALE_LOG", value),
            None => std::env::remove_var("TERNVALE_LOG"),
        }
    }
}
