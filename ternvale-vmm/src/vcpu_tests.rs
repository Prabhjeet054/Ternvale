use super::{exit_reason, ExitReason, Vcpu};

#[test]
fn maps_exit_reasons_without_a_vm() {
    let exception = exit_reason(
        ternvale_hv::HV_EXIT_REASON_EXCEPTION,
        0x5600_0000,
        0x10,
        0x4000_0000,
    );
    assert_eq!(
        exception,
        ExitReason::Exception {
            syndrome: 0x5600_0000,
            virtual_address: 0x10,
            physical_address: 0x4000_0000,
        }
    );
    assert_eq!(
        exit_reason(ternvale_hv::HV_EXIT_REASON_VTIMER_ACTIVATED, 0, 0, 0),
        ExitReason::VtimerActivated
    );
    assert_eq!(
        exit_reason(ternvale_hv::HV_EXIT_REASON_CANCELED, 0, 0, 0),
        ExitReason::Canceled
    );
    assert_eq!(
        exit_reason(ternvale_hv::HV_EXIT_REASON_UNKNOWN, 0, 0, 0),
        ExitReason::Unknown {
            reason: ternvale_hv::HV_EXIT_REASON_UNKNOWN
        }
    );
    assert_eq!(
        exit_reason(0x99, 0, 0, 0),
        ExitReason::Unknown { reason: 0x99 }
    );
}

#[test]
fn rejects_a_gpr_index_past_x30() {
    let error = ternvale_hv::Reg::X(31);
    let message = match ternvale_hv::get_reg(0, error) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("x31 was accepted"),
    };
    assert!(message.contains("31"), "{message}");
}

#[test]
#[ignore = "needs-hv"]
fn creates_vcpu_and_rejects_another_thread() {
    let vm = ternvale_hv::Vm::create().expect("vm");
    let vcpu = Vcpu::create(&vm).expect("vcpu");
    vcpu.set_pc(0x4000_0000).expect("set pc");
    assert_eq!(vcpu.get_pc().expect("get pc"), 0x4000_0000);
    vcpu.set_x(0, 0x11).expect("set x0");
    assert_eq!(vcpu.get_x(0).expect("get x0"), 0x11);
    vcpu.set_x(30, 0x1e).expect("set x30");
    assert_eq!(vcpu.get_x(30).expect("get x30"), 0x1e);
    vcpu.set_sp_el1(0x8000).expect("set sp");
    assert_eq!(vcpu.get_sp_el1().expect("get sp"), 0x8000);
    vcpu.set_cpsr(0x3c5).expect("set cpsr");
    assert_eq!(vcpu.get_cpsr().expect("get cpsr"), 0x3c5);
    vcpu.set_sys_reg(ternvale_hv::SysReg::VbarEl1, 0x1000)
        .expect("set vbar");
    assert_eq!(
        vcpu.get_sys_reg(ternvale_hv::SysReg::VbarEl1)
            .expect("get vbar"),
        0x1000
    );
    let stop = vcpu.stopper();
    let stopped = std::thread::spawn(move || {
        stop.request().expect("hv_vcpus_exit");
        stop.is_stopped()
    })
    .join()
    .expect("stop thread");
    assert!(stopped);
    let wrong =
        std::thread::scope(|scope| scope.spawn(|| vcpu.get_pc()).join().expect("other thread"));
    let message = wrong.expect_err("other thread should fail").to_string();
    assert!(message.contains("belongs to thread"), "{message}");
    drop(vcpu);
    drop(vm);
}
