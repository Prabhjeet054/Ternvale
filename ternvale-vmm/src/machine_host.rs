//! Stdin, the vCPU run loop, the hang watchdog thread, and parked secondary vCPUs.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::machine::MachineError;
use crate::mmio::MmioBus;
use crate::platform::RAM_BASE;
use crate::serial::SerialDevice;
use crate::vcpu::{ExitReason, Vcpu};
use crate::watchdog::Watchdog;

pub(super) fn park_vcpu(
    vm: &ternvale_hv::Vm,
    id: u32,
    shutdown: &AtomicBool,
    wake: &Condvar,
    parked: &Mutex<()>,
) -> Result<(), MachineError> {
    let vcpu = Vcpu::create(vm)?;
    tracing::warn!(
        target: "ternvale::vcpu",
        vcpu_id = vcpu.id(),
        cpu = id,
        "secondary vcpu is parked; CPU_ON does not start it"
    );
    let mut guard = parked.lock().unwrap_or_else(|poison| poison.into_inner());
    while !shutdown.load(Ordering::Acquire) {
        guard = wake
            .wait(guard)
            .unwrap_or_else(|poison| poison.into_inner());
    }
    drop(vcpu);
    Ok(())
}

pub(super) fn watch_loop(watchdog: Arc<Watchdog>, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_secs(1));
        if watchdog.poll().is_some() {
            tracing::debug!(target: "ternvale::vcpu", "hang watchdog fired");
        }
    }
}

pub(super) fn poll_loop(
    shutdown: Arc<AtomicBool>,
    kick: crate::vcpu::VcpuStop,
    gic: Arc<ternvale_hv::Gic>,
    irq_level: Arc<AtomicBool>,
) {
    let spi = 32 + crate::fdt::UART_SPI;
    while !shutdown.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(50));
        // hv_gic_set_spi(true) can be missed while the guest is inside an
        // emulated WFI. Pulse again only while the PL011 line is still high.
        if irq_level.load(Ordering::Acquire) {
            if let Err(error) = gic.set_spi(spi, true) {
                tracing::debug!(
                    target: "ternvale::gic",
                    error = %error,
                    "uart spi reassert failed"
                );
            }
        }
        if let Err(error) = kick.nudge() {
            tracing::debug!(target: "ternvale::vcpu", error = %error, "poll nudge failed");
        }
    }
}

pub(super) fn stdin_loop(tx: Sender<u8>, shutdown: Arc<AtomicBool>, kick: crate::vcpu::VcpuStop) {
    let fd = libc::STDIN_FILENO;
    // SAFETY: F_GETFL reads the stdin status flags. The fd is the process stdin.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        tracing::warn!(target: "ternvale::uart", "stdin stays blocking; rx thread not started");
        return;
    }
    // SAFETY: F_SETFL only adds O_NONBLOCK to the flags just read.
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc < 0 {
        tracing::warn!(target: "ternvale::uart", "could not set stdin nonblocking");
        return;
    }
    let mut buf = [0u8; 64];
    while !shutdown.load(Ordering::Acquire) {
        // SAFETY: `buf` is a writable 64-byte stack buffer. stdin is nonblocking.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            for byte in &buf[..n as usize] {
                if tx.send(*byte).is_err() {
                    break;
                }
            }
            if let Err(error) = kick.nudge() {
                tracing::warn!(target: "ternvale::uart", error = %error, "could not wake vcpu for stdin");
            }
        } else {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    // SAFETY: restore the flags captured before O_NONBLOCK was added.
    let restored = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    if restored < 0 {
        tracing::warn!(target: "ternvale::uart", "could not restore stdin flags");
    }
}

pub(super) fn run_loop(
    vcpu: &Vcpu,
    bus: &mut MmioBus,
    serial: &Rc<std::cell::RefCell<Box<dyn SerialDevice>>>,
    rx: &Receiver<u8>,
    watchdog: &Watchdog,
    memory: &mut crate::memory::GuestMemory,
    irq_level: &AtomicBool,
) -> Result<ExitReason, MachineError> {
    loop {
        drain_rx(serial, rx);
        if irq_level.load(Ordering::Acquire) {
            escape_masked_wfi(vcpu, memory)?;
        }
        let pc = vcpu.get_pc()?;
        watchdog.note_exit(pc);
        let reason = vcpu.run()?;
        let pc = vcpu.get_pc()?;
        watchdog.note_exit(pc);
        match reason {
            ExitReason::Exception {
                syndrome,
                physical_address,
                ..
            } => {
                if !dispatch_exit(vcpu, bus, watchdog, syndrome, physical_address)? {
                    return Ok(reason);
                }
            }
            ExitReason::Wfi | ExitReason::Canceled => {}
            ExitReason::SystemOff | ExitReason::SystemReset | ExitReason::CpuOff => {
                tracing::info!(target: "ternvale::boot", ?reason, "guest requested shutdown");
                return Ok(reason);
            }
            other => {
                tracing::error!(target: "ternvale::vcpu", ?other, "stopping on unexpected exit");
                return Ok(other);
            }
        }
    }
}

/// `false` when the exit is not MMIO or a sysreg trap and the run loop should stop.
fn dispatch_exit(
    vcpu: &Vcpu,
    bus: &mut MmioBus,
    watchdog: &Watchdog,
    syndrome: u64,
    physical_address: u64,
) -> Result<bool, MachineError> {
    let event = crate::esr::decode(syndrome, physical_address);
    match event {
        crate::esr::ExitEvent::Mmio {
            gpa, size, write, ..
        } => {
            watchdog.note_mmio(format!(
                "{} {gpa:#x} size={size}",
                if write { "write" } else { "read" }
            ));
            bus.dispatch(vcpu, event)?;
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

fn escape_masked_wfi(
    vcpu: &Vcpu,
    memory: &mut crate::memory::GuestMemory,
) -> Result<(), MachineError> {
    // hv_vcpu_run stays inside a WFI that began with PSTATE.I set. That
    // instruction is a nop when IRQs are masked, so a pending SPI never wakes it.
    let pc = vcpu.get_pc()?;
    for wfi_pc in [pc, pc.wrapping_sub(4)] {
        if guest_insn(memory, wfi_pc) != Some(0xd503_207f) {
            continue;
        }
        let phys = RAM_BASE + (wfi_pc - 0xffff_8000_8000_0000);
        memory.write_bytes(phys, &0xd503_201f_u32.to_le_bytes())?;
        vcpu.set_pc(wfi_pc)?;
        let cpsr = vcpu.get_cpsr()?;
        if cpsr & 0x80 != 0 {
            vcpu.set_cpsr(cpsr & !0x80)?;
        }
        tracing::info!(
            target: "ternvale::vcpu",
            pc = format!("{wfi_pc:#x}"),
            "replaced masked wfi with nop"
        );
        break;
    }
    Ok(())
}

fn guest_insn(memory: &crate::memory::GuestMemory, pc: u64) -> Option<u32> {
    const KIMAGE: u64 = 0xffff_8000_8000_0000;
    if pc < KIMAGE {
        return None;
    }
    let phys = RAM_BASE.checked_add(pc - KIMAGE)?;
    let mut buf = [0u8; 4];
    memory.read_bytes(phys, &mut buf).ok()?;
    Some(u32::from_le_bytes(buf))
}

fn drain_rx(serial: &Rc<std::cell::RefCell<Box<dyn SerialDevice>>>, rx: &Receiver<u8>) {
    while let Ok(byte) = rx.try_recv() {
        if !serial.borrow_mut().push_rx(byte) {
            tracing::warn!(target: "ternvale::uart", "dropped stdin byte");
        }
    }
}
