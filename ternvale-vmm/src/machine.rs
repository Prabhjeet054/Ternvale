//! Boots a configured guest and runs it until PSCI powers it off.
//!
//! Order: config, guest RAM, GIC, UART on the MMIO bus, DTB, Linux loader,
//! then one thread per vCPU. Host stdin is queued to the UART. The boot thread
//! drains that queue and raises GIC SPI 1. A watchdog warns if no exit arrives
//! for 10 seconds.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[path = "machine_host.rs"]
mod host;

use crate::fdt::{build_fdt, GuestFdt, PL011_REG_SIZE};
use crate::gic_redist::RedistId;
use crate::linux::{load_linux, place, LinuxLayout};
use crate::mmio::{MmioBus, MmioDevice};
use crate::platform::{GIC_DIST_BASE, GIC_REDIST_BASE, GIC_REDIST_SIZE, RAM_BASE, UART_BASE};
use crate::serial::SerialDevice;
use crate::vcpu::{ExitReason, Vcpu};
use crate::watchdog::Watchdog;

/// Kernel command line used when the config leaves it empty.
pub const DEFAULT_CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init";

const HANG: Duration = Duration::from_secs(10);

/// `cmdline`, or [`DEFAULT_CMDLINE`] when it is empty.
#[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
pub fn guest_cmdline(cmdline: &str) -> String {
    if cmdline.is_empty() {
        tracing::info!(target: "ternvale::boot", cmdline = DEFAULT_CMDLINE, "default kernel cmdline");
        DEFAULT_CMDLINE.to_string()
    } else {
        tracing::info!(target: "ternvale::boot", cmdline, "kernel cmdline");
        cmdline.to_string()
    }
}

/// Why the machine did not finish booting or running.
#[derive(Debug, thiserror::Error)]
pub enum MachineError {
    /// The config document was rejected.
    #[error("vm config: {0}")]
    Config(#[from] ternvale_config::ConfigError),
    /// The hypervisor rejected VM or GIC setup.
    #[error("vm hypervisor: {0}")]
    Hv(#[from] ternvale_hv::HvError),
    /// Guest RAM could not be mapped.
    #[error("vm memory: {0}")]
    Memory(#[from] crate::memory::MemoryError),
    /// The kernel, initrd, or DTB could not be placed.
    #[error("vm linux boot: {0}")]
    Linux(#[from] crate::linux::LinuxBootError),
    /// The DTB could not be built.
    #[error("vm dtb: {0}")]
    Fdt(#[from] crate::fdt::FdtError),
    /// A vCPU call failed.
    #[error("vm vcpu: {0}")]
    Vcpu(#[from] crate::vcpu::VcpuError),
    /// MMIO dispatch failed.
    #[error("vm mmio: {0}")]
    Mmio(#[from] crate::mmio::MmioError),
    /// A kernel or initrd file could not be read.
    #[error("read {what} {}: {source}", path.display())]
    Read {
        /// `kernel` or `initrd`.
        what: &'static str,
        /// Host path.
        path: std::path::PathBuf,
        /// OS error.
        source: std::io::Error,
    },
    /// The DTB and the Linux placement did not converge.
    #[error("dtb placement did not settle")]
    Placement,
}

/// One guest. [`Machine::run`] owns the hypervisor VM until the guest stops.
pub struct Machine;

impl Machine {
    /// Build the machine from `config`, run until shutdown, and destroy the VM.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(name = %config.name))]
    pub fn run(
        config: &ternvale_config::VmConfig,
        serial: Box<dyn SerialDevice>,
    ) -> Result<ExitReason, MachineError> {
        config.validate()?;
        let cmdline = guest_cmdline(&config.cmdline);
        tracing::info!(
            target: "ternvale::boot",
            name = %config.name,
            cpus = config.cpus,
            ram_mib = config.ram_mib,
            "starting vm"
        );
        if !config.disks.is_empty() || !config.nics.is_empty() {
            tracing::warn!(
                target: "ternvale::boot",
                disks = config.disks.len(),
                nics = config.nics.len(),
                "disks and nics are not attached"
            );
        }
        let kernel = read_file("kernel", &config.kernel)?;
        let initrd = match &config.initrd {
            Some(path) => read_file("initrd", path)?,
            None => Vec::new(),
        };
        let ram_size = config.ram_mib << 20;
        let vm = ternvale_hv::Vm::create()?;
        let gic = Arc::new(vm.create_gic(GIC_DIST_BASE, GIC_REDIST_BASE)?);
        let mut memory = crate::memory::GuestMemory::new()?;
        memory.map(&vm, RAM_BASE, ram_size)?;
        let (dtb, layout) = boot_images(&cmdline, config.cpus, ram_size, &kernel, &initrd)?;
        tracing::debug!(
            target: "ternvale::boot",
            kernel = format!("{:#x}", layout.kernel),
            dtb = format!("{:#x}", layout.dtb),
            "boot images placed"
        );
        let serial = Rc::new(std::cell::RefCell::new(serial));
        let irq_level = Arc::new(AtomicBool::new(false));
        install_uart_irq(Rc::clone(&serial), Arc::clone(&gic), Arc::clone(&irq_level));
        let mut bus = MmioBus::new();
        bus.register(GIC_REDIST_BASE, GIC_REDIST_SIZE, Box::new(RedistId))?;
        bus.register(
            UART_BASE,
            PL011_REG_SIZE,
            Box::new(LocalUart(Rc::clone(&serial))),
        )?;
        let (rx_tx, rx_rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Condvar::new());
        let parked = Arc::new(Mutex::new(()));
        let watchdog = Arc::new(Watchdog::new(HANG));
        let stop_watch = Arc::clone(&shutdown);
        let dog = Arc::clone(&watchdog);
        let _watch = std::thread::spawn(move || host::watch_loop(dog, stop_watch));
        let exit = std::thread::scope(|scope| {
            let mut stops = Vec::new();
            for id in 1..config.cpus {
                let vm = &vm;
                let shutdown = Arc::clone(&shutdown);
                let wake = Arc::clone(&wake);
                let parked = Arc::clone(&parked);
                stops.push(scope.spawn(move || host::park_vcpu(vm, id, &shutdown, &wake, &parked)));
            }
            let images = Images {
                kernel: &kernel,
                initrd: &initrd,
                dtb: &dtb,
                ram_size,
            };
            let exit = boot_vcpu(
                &vm,
                &mut memory,
                &mut bus,
                &serial,
                &watchdog,
                &images,
                BootIo {
                    rx_tx,
                    rx: rx_rx,
                    shutdown: Arc::clone(&shutdown),
                    gic: Arc::clone(&gic),
                    irq_level: Arc::clone(&irq_level),
                },
            );
            shutdown.store(true, Ordering::Release);
            wake.notify_all();
            for handle in stops {
                match handle.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::error!(target: "ternvale::vcpu", error = %error, "secondary vcpu failed");
                    }
                    Err(_) => {
                        tracing::error!(target: "ternvale::vcpu", "secondary vcpu thread panicked");
                    }
                }
            }
            exit
        })?;
        tracing::info!(target: "ternvale::boot", ?exit, "vm stopped");
        drop(bus);
        drop(serial);
        drop(memory);
        drop(gic);
        drop(vm);
        Ok(exit)
    }
}

fn boot_images(
    cmdline: &str,
    cpus: u32,
    ram_size: u64,
    kernel: &[u8],
    initrd: &[u8],
) -> Result<(Vec<u8>, LinuxLayout), MachineError> {
    let header = crate::linux::parse_header(kernel)?;
    let mut dtb;
    let mut layout = None;
    for _ in 0..3 {
        let guess = layout.unwrap_or(LinuxLayout {
            kernel: 0,
            kernel_bytes: 0,
            initrd: 0,
            initrd_bytes: initrd.len() as u64,
            dtb: 0,
            dtb_bytes: 0,
        });
        dtb = build_fdt(&GuestFdt {
            bootargs: cmdline.to_string(),
            ram_base: RAM_BASE,
            ram_size,
            initrd_start: guess.initrd,
            initrd_end: guess.initrd.saturating_add(initrd.len() as u64),
            cpu_count: cpus,
        })?;
        let next = place(
            RAM_BASE,
            ram_size,
            &header,
            kernel.len() as u64,
            initrd.len() as u64,
            dtb.len() as u64,
        )?;
        if layout == Some(next) {
            return Ok((dtb, next));
        }
        layout = Some(next);
    }
    tracing::error!(target: "ternvale::boot", "dtb placement did not settle");
    Err(MachineError::Placement)
}

fn install_uart_irq(
    serial: Rc<std::cell::RefCell<Box<dyn SerialDevice>>>,
    gic: Arc<ternvale_hv::Gic>,
    level_flag: Arc<AtomicBool>,
) {
    let spi = 32 + crate::fdt::UART_SPI;
    serial.borrow_mut().set_irq_hook(Arc::new(move |level| {
        // set_spi(true) also pulses an edge. Repeat calls while the line stays
        // high leave a pending SPI after the PL011 has cleared MIS.
        if level_flag.swap(level, Ordering::AcqRel) == level {
            return;
        }
        if let Err(error) = gic.set_spi(spi, level) {
            tracing::warn!(
                target: "ternvale::gic",
                irq = spi,
                level,
                error = %error,
                "uart spi update failed"
            );
        }
    }));
}

struct LocalUart(Rc<std::cell::RefCell<Box<dyn SerialDevice>>>);

impl MmioDevice for LocalUart {
    fn name(&self) -> &str {
        "pl011"
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        self.0.borrow_mut().read(offset, size)
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        self.0.borrow_mut().write(offset, size, val);
    }
}

struct Images<'a> {
    kernel: &'a [u8],
    initrd: &'a [u8],
    dtb: &'a [u8],
    ram_size: u64,
}

struct BootIo {
    rx_tx: std::sync::mpsc::Sender<u8>,
    rx: Receiver<u8>,
    shutdown: Arc<AtomicBool>,
    gic: Arc<ternvale_hv::Gic>,
    irq_level: Arc<AtomicBool>,
}

fn boot_vcpu(
    vm: &ternvale_hv::Vm,
    memory: &mut crate::memory::GuestMemory,
    bus: &mut MmioBus,
    serial: &Rc<std::cell::RefCell<Box<dyn SerialDevice>>>,
    watchdog: &Watchdog,
    images: &Images<'_>,
    io: BootIo,
) -> Result<ExitReason, MachineError> {
    let vcpu = Vcpu::create(vm)?;
    // Affinity 0 with the RES1 bit. The framework will not bind a redistributor
    // to this vCPU until MPIDR_EL1 is written, so SPIs routed at affinity 0
    // never reach the CPU interface.
    vcpu.set_sys_reg(ternvale_hv::SysReg::MpidrEl1, 0x8000_0000)?;
    match io.gic.vcpu_redistributor_base(vcpu.id()) {
        Ok(base) => tracing::info!(
            target: "ternvale::gic",
            base = format!("{base:#x}"),
            "vcpu redistributor"
        ),
        Err(error) => tracing::warn!(
            target: "ternvale::gic",
            error = %error,
            "vcpu redistributor base unavailable"
        ),
    }
    let kick = vcpu.stopper();
    let poll_kick = vcpu.stopper();
    let poll_stop = Arc::clone(&io.shutdown);
    let level_run = Arc::clone(&io.irq_level);
    let rx = io.rx;
    let _stdin = std::thread::spawn(move || host::stdin_loop(io.rx_tx, io.shutdown, kick));
    let _poll =
        std::thread::spawn(move || host::poll_loop(poll_stop, poll_kick, io.gic, io.irq_level));
    load_linux(
        memory,
        &vcpu,
        RAM_BASE,
        images.ram_size,
        images.kernel,
        images.initrd,
        images.dtb,
    )?;
    let exit = host::run_loop(&vcpu, bus, serial, &rx, watchdog, memory, &level_run)?;
    drop(vcpu);
    Ok(exit)
}

fn read_file(what: &'static str, path: &std::path::Path) -> Result<Vec<u8>, MachineError> {
    std::fs::read(path).map_err(|source| {
        tracing::error!(target: "ternvale::boot", what, path = %path.display(), error = %source, "failed to read boot file");
        MachineError::Read {
            what,
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{guest_cmdline, DEFAULT_CMDLINE};

    #[test]
    fn empty_cmdline_uses_the_pl011_console_default() {
        assert_eq!(guest_cmdline(""), DEFAULT_CMDLINE);
        assert_eq!(guest_cmdline("console=ttyAMA0"), "console=ttyAMA0");
    }
}
