//! One host thread per guest CPU.
//!
//! Hypervisor.framework binds a vCPU to the thread that created it, so each
//! thread creates, configures, runs, and destroys its own vCPU. CPU 0 loads
//! Linux and starts once every vCPU exists. The others wait powered off until
//! PSCI `CPU_ON`. Every thread runs inside a `vcpu` span carrying `vm`, `cpu`,
//! `mpidr`, and `vcpu_id`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use ternvale_hv::SysReg;

use super::attach::SpiLevels;
use super::{Images, MachineError};
use crate::gic_redist::RedistMap;
use crate::linux::{load_linux, CPSR_EL1H_MASKED};
use crate::memory::GuestMemory;
use crate::mmio::MmioBus;
use crate::platform::RAM_BASE;
use crate::smp::{self, CpuPower};
use crate::vcpu::{ExitReason, Vcpu, VcpuError};
use crate::watchdog::Watchdog;

const SCTLR_M: u64 = 1 << 0;
const SCTLR_C: u64 = 1 << 2;

/// State every vCPU thread shares. Lock order is in `docs/ARCHITECTURE.md`.
pub(super) struct Shared<'a> {
    pub name: &'a str,
    pub vm: &'a ternvale_hv::Vm,
    pub gic: &'a ternvale_hv::Gic,
    pub memory: &'a Mutex<GuestMemory>,
    pub bus: &'a MmioBus,
    pub power: &'a CpuPower,
    pub redist: &'a RedistMap,
    pub watchdog: &'a Watchdog,
    pub irq_level: &'a AtomicBool,
    pub spi_levels: &'a SpiLevels,
    pub images: &'a Images<'a>,
    /// CPU 0's vtimer offset. Each secondary copies it before its first entry
    /// so every CPU reads the same `CNTVCT_EL0`.
    pub vtimer_offset: &'a OnceLock<u64>,
}

enum Leave {
    CpuOff,
    Stop,
}

/// Body of the thread for guest CPU `index`.
pub(super) fn vcpu_thread(index: u32, shared: &Shared<'_>) -> Result<(), MachineError> {
    let span = tracing::info_span!(
        target: "ternvale::vcpu",
        "vcpu",
        vm = %shared.name,
        cpu = index,
        mpidr = %format!("{:#x}", smp::mpidr(index)),
    );
    let _entered = span.enter();
    let _panic = StopOnPanic(shared.power);
    let result = own_vcpu(index, shared);
    if let Err(error) = &result {
        tracing::error!(target: "ternvale::vcpu", cpu = index, error = %error, "vcpu thread failed; stopping the vm");
        shared.power.request_stop(ExitReason::Canceled);
    }
    tracing::info!(target: "ternvale::vcpu", cpu = index, "vcpu thread exiting");
    result
}

/// Stops the VM if the thread unwinds, so no other thread waits forever.
struct StopOnPanic<'a>(&'a CpuPower);

impl Drop for StopOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            tracing::error!(target: "ternvale::vcpu", "vcpu thread panicked; stopping the vm");
            self.0.request_stop(ExitReason::Canceled);
        }
    }
}

/// Deregisters the vCPU before it is destroyed (declared after the `Vcpu`, so
/// it drops first, also on unwind).
struct Registered<'a> {
    power: &'a CpuPower,
    index: u32,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.power.deregister(self.index);
    }
}

fn own_vcpu(index: u32, shared: &Shared<'_>) -> Result<(), MachineError> {
    if !shared.power.wait_create_turn(index) {
        return Ok(());
    }
    let created = Vcpu::create(shared.vm);
    shared.power.mark_created(index);
    let vcpu = created.map_err(op(index, "create vcpu"))?;
    // A nested span rather than a late `record`: with the stderr and file
    // layers both formatting, a recorded field is printed twice.
    let _id = tracing::info_span!(target: "ternvale::vcpu", "run", vcpu_id = vcpu.id()).entered();
    let _registered = Registered {
        power: shared.power,
        index,
    };
    if !setup(index, &vcpu, shared)? {
        return Ok(());
    }
    // CPU 0 is already at the kernel entry; every other start waits for CPU_ON.
    let mut powered_off = index != 0;
    loop {
        if powered_off {
            let Some((entry, context)) = shared.power.wait_for_on(index) else {
                return Ok(());
            };
            enter_at(index, &vcpu, shared, entry, context)?;
        }
        match run_loop(index, &vcpu, shared)? {
            Leave::CpuOff => {
                shared.power.cpu_off(index);
                powered_off = true;
            }
            Leave::Stop => return Ok(()),
        }
    }
}

/// `false` when the VM stopped before this CPU could start.
fn setup(index: u32, vcpu: &Vcpu, shared: &Shared<'_>) -> Result<bool, MachineError> {
    let mpidr = smp::mpidr(index);
    // The framework binds a redistributor only after MPIDR_EL1 is written;
    // until then SPIs routed to this affinity never reach the CPU.
    vcpu.set_sys_reg(SysReg::MpidrEl1, mpidr)
        .map_err(op(index, "set MPIDR_EL1"))?;
    match shared.gic.vcpu_redistributor_base(vcpu.id()) {
        Ok(base) => {
            tracing::info!(
                target: "ternvale::gic",
                cpu = index,
                vcpu_id = vcpu.id(),
                mpidr = format!("{mpidr:#x}"),
                base = format!("{base:#x}"),
                "vcpu redistributor"
            );
            shared.redist.record(index, mpidr, base);
        }
        Err(error) => tracing::warn!(
            target: "ternvale::gic",
            cpu = index,
            error = %error,
            "vcpu redistributor base unavailable; assuming frame {index}"
        ),
    }
    let offset = vcpu
        .vtimer_offset()
        .map_err(op(index, "read vtimer offset"))?;
    tracing::debug!(target: "ternvale::vcpu", cpu = index, offset = format!("{offset:#x}"), "vtimer offset at create");
    if index == 0 && shared.vtimer_offset.set(offset).is_err() {
        tracing::warn!(target: "ternvale::vcpu", "boot vtimer offset was already published");
    }
    if !shared.power.register(index, vcpu.stopper()) {
        tracing::info!(target: "ternvale::vcpu", cpu = index, "vm stopped before this vcpu started");
        return Ok(false);
    }
    if index == 0 {
        let mut mem = crate::lockwatch::lock(shared.memory, "guest-memory");
        let images = shared.images;
        load_linux(
            &mut mem,
            vcpu,
            RAM_BASE,
            images.ram_size,
            images.kernel,
            images.initrd,
            images.dtb,
        )?;
    }
    shared.power.mark_ready(index);
    tracing::info!(
        target: "ternvale::vcpu",
        cpu = index,
        vcpu_id = vcpu.id(),
        mpidr = format!("{mpidr:#x}"),
        powered_on = index == 0,
        "vcpu ready"
    );
    if index == 0 {
        if !shared.power.wait_all_ready() {
            return Ok(false);
        }
        tracing::info!(target: "ternvale::boot", cpus = shared.power.count(), "every vcpu is ready; starting the boot cpu");
    }
    Ok(true)
}

/// PSCI `CPU_ON` state: PC = entry, x0 = context, EL1h with DAIF masked, MMU
/// and data cache off (they may still be on from before a `CPU_OFF`).
fn enter_at(
    index: u32,
    vcpu: &Vcpu,
    shared: &Shared<'_>,
    entry: u64,
    context: u64,
) -> Result<(), MachineError> {
    if index != 0 {
        if let Some(&offset) = shared.vtimer_offset.get() {
            vcpu.set_vtimer_offset(offset)
                .map_err(op(index, "set vtimer offset"))?;
        } else {
            tracing::warn!(target: "ternvale::vcpu", cpu = index, "boot vtimer offset unknown; keeping this vcpu's own");
        }
    }
    let sctlr = vcpu
        .get_sys_reg(SysReg::SctlrEl1)
        .map_err(op(index, "read SCTLR_EL1"))?;
    let cleared = sctlr & !(SCTLR_M | SCTLR_C);
    if cleared != sctlr {
        vcpu.set_sys_reg(SysReg::SctlrEl1, cleared)
            .map_err(op(index, "clear SCTLR_EL1.M/C"))?;
    }
    vcpu.set_x(0, context)
        .map_err(op(index, "set x0 context"))?;
    vcpu.set_pc(entry).map_err(op(index, "set entry pc"))?;
    vcpu.set_cpsr(CPSR_EL1H_MASKED)
        .map_err(op(index, "set entry cpsr"))?;
    tracing::info!(
        target: "ternvale::psci",
        cpu = index,
        entry = format!("{entry:#x}"),
        context = format!("{context:#x}"),
        sctlr = format!("{cleared:#x}"),
        "vcpu entering at the cpu_on entry point"
    );
    Ok(())
}

fn run_loop(index: u32, vcpu: &Vcpu, shared: &Shared<'_>) -> Result<Leave, MachineError> {
    loop {
        let virtio_pending = crate::lockwatch::lock(shared.spi_levels, "spi-levels")
            .iter()
            .any(|(_, level)| level.load(Ordering::Acquire));
        if shared.irq_level.load(Ordering::Acquire) || virtio_pending {
            escape_masked_wfi(vcpu, shared.memory)?;
        }
        let pc = vcpu.get_pc().map_err(op(index, "read pc"))?;
        shared.watchdog.note_exit(pc);
        let reason = vcpu.run().map_err(op(index, "run"))?;
        let pc = vcpu.get_pc().map_err(op(index, "read pc"))?;
        shared.watchdog.note_exit(pc);
        match reason {
            ExitReason::Exception {
                syndrome,
                physical_address,
                ..
            } => {
                if !dispatch_exit(vcpu, shared, syndrome, physical_address)? {
                    shared.power.request_stop(reason);
                    return Ok(Leave::Stop);
                }
            }
            ExitReason::Wfi => {}
            ExitReason::Canceled => {
                if let Some(stop) = shared.power.stop_reason() {
                    if stop == ExitReason::Canceled {
                        tracing::warn!(target: "ternvale::boot", cpu = index, "guest run cancelled");
                    }
                    return Ok(Leave::Stop);
                }
            }
            ExitReason::Psci(request) => {
                let x0 = shared.power.handle(index, request);
                vcpu.set_x(0, x0).map_err(op(index, "write psci status"))?;
            }
            ExitReason::CpuOff => return Ok(Leave::CpuOff),
            ExitReason::SystemOff | ExitReason::SystemReset => {
                tracing::info!(target: "ternvale::boot", cpu = index, ?reason, "guest requested shutdown");
                shared.power.request_stop(reason);
                return Ok(Leave::Stop);
            }
            other => {
                tracing::error!(target: "ternvale::vcpu", cpu = index, ?other, "stopping on unexpected exit");
                shared.power.request_stop(other);
                return Ok(Leave::Stop);
            }
        }
    }
}

/// `false` when the exit is not MMIO or a sysreg trap and the VM should stop.
fn dispatch_exit(
    vcpu: &Vcpu,
    shared: &Shared<'_>,
    syndrome: u64,
    physical_address: u64,
) -> Result<bool, MachineError> {
    let event = crate::esr::decode(syndrome, physical_address);
    match event {
        crate::esr::ExitEvent::Mmio {
            gpa, size, write, ..
        } => {
            shared.watchdog.note_mmio(format!(
                "{} {gpa:#x} size={size}",
                if write { "write" } else { "read" }
            ));
            shared.bus.dispatch(vcpu, event)?;
            let pc = vcpu.get_pc()?;
            vcpu.set_pc(pc + 4)?;
            Ok(true)
        }
        crate::esr::ExitEvent::SysReg { reg, write, .. } => {
            // The guest PC stays on the MSR/MRS, as it does for a data abort.
            tracing::debug!(
                target: "ternvale::vcpu",
                vcpu_id = vcpu.id(),
                reg,
                write,
                "sysreg trap"
            );
            if !write && reg <= 30 {
                vcpu.set_x(reg, 0)?;
            }
            let pc = vcpu.get_pc()?;
            vcpu.set_pc(pc + 4)?;
            Ok(true)
        }
        other => {
            tracing::error!(
                target: "ternvale::vcpu",
                vcpu_id = vcpu.id(),
                ?other,
                "unhandled guest exit"
            );
            Ok(false)
        }
    }
}

fn escape_masked_wfi(vcpu: &Vcpu, memory: &Mutex<GuestMemory>) -> Result<(), MachineError> {
    // hv_vcpu_run stays inside a WFI that began with PSTATE.I set. That
    // instruction is a nop when IRQs are masked, so a pending SPI never wakes it.
    let pc = vcpu.get_pc()?;
    let mut mem = crate::lockwatch::lock(memory, "guest-memory");
    for wfi_pc in [pc, pc.wrapping_sub(4)] {
        if guest_insn(&mem, wfi_pc) != Some(0xd503_207f) {
            continue;
        }
        let phys = RAM_BASE + (wfi_pc - 0xffff_8000_8000_0000);
        mem.write_bytes(phys, &0xd503_201f_u32.to_le_bytes())?;
        vcpu.set_pc(wfi_pc)?;
        let cpsr = vcpu.get_cpsr()?;
        if cpsr & 0x80 != 0 {
            vcpu.set_cpsr(cpsr & !0x80)?;
        }
        tracing::info!(
            target: "ternvale::vcpu",
            vcpu_id = vcpu.id(),
            pc = format!("{wfi_pc:#x}"),
            "replaced masked wfi with nop"
        );
        break;
    }
    Ok(())
}

fn guest_insn(memory: &GuestMemory, pc: u64) -> Option<u32> {
    const KIMAGE: u64 = 0xffff_8000_8000_0000;
    if pc < KIMAGE {
        return None;
    }
    let phys = RAM_BASE.checked_add(pc - KIMAGE)?;
    let mut buf = [0u8; 4];
    memory.read_bytes(phys, &mut buf).ok()?;
    Some(u32::from_le_bytes(buf))
}

fn op(cpu: u32, what: &'static str) -> impl FnOnce(VcpuError) -> MachineError {
    move |source| MachineError::VcpuOp { cpu, what, source }
}
