//! A guest `MRS x0, PMCR_EL0` at EL1 completes once the vCPU advertises PMUv3.
//! Without it, Hypervisor.framework injects an Undefined Instruction exception
//! (the Windows `cdboot.efi` halt).

use crate::vcpu::{ExitReason, Vcpu};
use crate::{GuestMemory, GIC_DIST_BASE, GIC_REDIST_BASE, HOST_PAGE_SIZE};
use ternvale_hv::SysReg;

const GUEST_PC: u64 = 0x4000_0000;
const VBAR: u64 = GUEST_PC + 0x800;
const MRS_X0_PMCR_EL0: u32 = 0xd53b_9c00;
/// Reached after the read completes.
const HVC0: u32 = 0xd400_0002;
/// The synchronous current-EL SP_ELx vector: reached if the read faults.
const HVC1: u32 = 0xd400_0022;
const CPSR_EL1H: u64 = 0x3c5;
/// Not a PSCI function ID, so `Vcpu::run` returns the HVC as an exception exit.
const NOT_PSCI: u64 = 0xdead;
const EC_HVC64: u64 = 0x16;

#[test]
#[ignore = "needs-hv"]
fn pmcr_el0_read_completes_with_pmuv3_advertised() {
    let vm = ternvale_hv::Vm::create().expect("vm");
    vm.create_gic(GIC_DIST_BASE, GIC_REDIST_BASE).expect("gic");
    let mut memory = GuestMemory::new().expect("memory");
    memory.map(&vm, GUEST_PC, HOST_PAGE_SIZE).expect("map");
    memory.write_u32(GUEST_PC, MRS_X0_PMCR_EL0).expect("mrs");
    memory.write_u32(GUEST_PC + 4, HVC0).expect("hvc0");
    memory.write_u32(VBAR + 0x200, HVC1).expect("hvc1");

    let vcpu = Vcpu::create(&vm).expect("vcpu");
    crate::pmu::advertise_pmuv3(&vcpu).expect("advertise pmuv3");
    let dfr0 = vcpu.get_sys_reg(SysReg::IdAa64Dfr0El1).expect("dfr0");
    assert_ne!((dfr0 >> 8) & 0xf, 0, "PMUVer still zero: {dfr0:#x}");
    vcpu.set_sys_reg(SysReg::VbarEl1, VBAR).expect("vbar");
    vcpu.set_cpsr(CPSR_EL1H).expect("cpsr");
    vcpu.set_pc(GUEST_PC).expect("pc");
    vcpu.set_x(0, NOT_PSCI).expect("x0");

    let exit = vcpu.run().expect("run");
    let ExitReason::Exception { syndrome, .. } = exit else {
        panic!("expected an hvc exit, got {exit:?}");
    };
    assert_eq!((syndrome >> 26) & 0x3f, EC_HVC64, "esr {syndrome:#x}");
    let esr_el1 = vcpu.get_sys_reg(SysReg::EsrEl1).expect("esr_el1");
    assert_eq!(
        syndrome & 0xffff,
        0,
        "the PMCR_EL0 read faulted into the guest vector, ESR_EL1={esr_el1:#x}"
    );
    let x0 = vcpu.get_x(0).expect("x0");
    assert_ne!(x0, NOT_PSCI, "PMCR_EL0 read did not write x0");
}
