//! Boots a configured guest and runs it until PSCI powers it off.
//!
//! Order: config, guest RAM (plus the firmware code bank; ACPI tables and fw_cfg when
//! `firmware_tables = "acpi"`), GIC, attached devices, UART, PL031, variable flash, PCI ECAM/BAR
//! windows on the MMIO bus, DTB, then one host thread per vCPU
//! (`machine_vcpu.rs`). CPU 0 loads Linux or enters UEFI at GPA 0
//! (`machine_images.rs`); the others start on PSCI `CPU_ON`. Host stdin goes straight into the shared UART, which raises
//! GIC SPI 1. A watchdog warns if no exit arrives for 10 seconds. Any vCPU can
//! end the VM; [`CpuPower`] then cancels every vCPU with one `hv_vcpus_exit`.
//! A [`VmControl`] follows the lifecycle (`Created → Running ⇄ Paused →
//! Stopping → Stopped | Failed`) and pauses vCPUs between runs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

#[path = "machine_attach.rs"]
mod attach;
#[path = "machine_host.rs"]
mod host;
#[path = "machine_images.rs"]
mod images;
#[path = "machine_vcpu.rs"]
mod vcpu_thread;

pub use attach::DeviceAttach;

#[path = "boot_cmdline.rs"]
pub(crate) mod cmdline;
pub use cmdline::{guest_cmdline, guest_cmdline_for, DEFAULT_CMDLINE, DISK_ROOT_CMDLINE};

use crate::control::VmControl;
use crate::fdt::PL011_REG_SIZE;
use crate::gic_redist::{RedistId, RedistMap};
use crate::mmio::{MmioBus, MmioDevice};
use crate::platform::{
    FLASH_BANK_SIZE, FLASH_VARS_BASE, GIC_DIST_BASE, GIC_REDIST_BASE, GIC_REDIST_SIZE, RAM_BASE,
    RTC_BASE, UART_BASE,
};
use crate::rtc::{Pl031, PL031_REG_SIZE};
use crate::serial::SerialDevice;
use crate::smp::CpuPower;
use crate::vcpu::ExitReason;
use crate::watchdog::Watchdog;
use images::{Images, Inputs};

const HANG: Duration = Duration::from_secs(10);

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
    /// A named vCPU operation on guest CPU `cpu` failed.
    #[error("vm cpu {cpu} {what}: {source}")]
    VcpuOp {
        /// Guest CPU index.
        cpu: u32,
        /// Operation, for example `set MPIDR_EL1`.
        what: &'static str,
        /// Underlying vCPU error.
        source: crate::vcpu::VcpuError,
    },
    /// A vCPU host thread could not be started.
    #[error("spawn vcpu {cpu} thread: {source}")]
    Spawn {
        /// Guest CPU index.
        cpu: u32,
        /// OS error.
        source: std::io::Error,
    },
    /// A vCPU host thread panicked. The panic hook logged the message.
    #[error("vcpu {cpu} thread panicked")]
    VcpuPanic {
        /// Guest CPU index.
        cpu: u32,
    },
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
    /// Attaching an MMIO device failed.
    #[error("attach devices: {0}")]
    Attach(String),
    /// Adding a PCI function or mapping the PCI windows failed.
    #[error("vm pci: {0}")]
    Pci(#[from] crate::pci::PciError),
    /// Loading the UEFI firmware or opening its variable store failed.
    #[error("vm firmware: {0}")]
    Firmware(#[from] crate::firmware::FirmwareError),
    /// The ACPI tables could not be built or written.
    #[error("vm acpi: {0}")]
    Acpi(#[from] ternvale_acpi::AcpiError),
    /// The lifecycle controller does not match the config.
    #[error("vm control: {0}")]
    Control(String),
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
        Self::run_until(config, serial, Arc::new(AtomicBool::new(false)))
    }

    /// [`Machine::run`], stopping the guest when `cancel` becomes true.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(name = %config.name))]
    pub fn run_until(
        config: &ternvale_config::VmConfig,
        serial: Box<dyn SerialDevice>,
        cancel: Arc<AtomicBool>,
    ) -> Result<ExitReason, MachineError> {
        Self::run_with(config, serial, cancel, |_| Ok(Vec::new()))
    }

    /// [`Machine::run_until`] plus devices built after guest RAM and the GIC exist.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::boot",
        skip_all,
        fields(name = %config.name)
    )]
    pub fn run_with<F>(
        config: &ternvale_config::VmConfig,
        serial: Box<dyn SerialDevice>,
        cancel: Arc<AtomicBool>,
        attach: F,
    ) -> Result<ExitReason, MachineError>
    where
        F: FnOnce(&DeviceAttach) -> Result<Vec<(u64, u64, Box<dyn MmioDevice>)>, MachineError>,
    {
        config.validate()?;
        let control = Arc::new(VmControl::new(&config.name, config.cpus));
        Self::run_tracked(config, serial, cancel, &control, attach)
    }

    /// [`Machine::run_with`] driven by `control`: its state follows the run,
    /// and its pause, resume, and stop requests act on the vCPUs. `control`
    /// must track `config.cpus` CPUs.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::boot",
        skip_all,
        fields(name = %config.name)
    )]
    pub fn run_controlled<F>(
        config: &ternvale_config::VmConfig,
        serial: Box<dyn SerialDevice>,
        control: &Arc<VmControl>,
        attach: F,
    ) -> Result<ExitReason, MachineError>
    where
        F: FnOnce(&DeviceAttach) -> Result<Vec<(u64, u64, Box<dyn MmioDevice>)>, MachineError>,
    {
        if control.cpus() != config.cpus {
            let error = MachineError::Control(format!(
                "controller tracks {} cpus, config has {}",
                control.cpus(),
                config.cpus
            ));
            control.finish(Err(error.to_string()));
            return Err(error);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        Self::run_tracked(config, serial, cancel, control, attach)
    }

    fn run_tracked<F>(
        config: &ternvale_config::VmConfig,
        serial: Box<dyn SerialDevice>,
        cancel: Arc<AtomicBool>,
        control: &Arc<VmControl>,
        attach: F,
    ) -> Result<ExitReason, MachineError>
    where
        F: FnOnce(&DeviceAttach) -> Result<Vec<(u64, u64, Box<dyn MmioDevice>)>, MachineError>,
    {
        let result = Self::run_inner(config, serial, cancel, control, attach);
        control.finish(result.as_ref().map(|exit| *exit).map_err(|e| e.to_string()));
        result
    }

    fn run_inner<F>(
        config: &ternvale_config::VmConfig,
        serial: Box<dyn SerialDevice>,
        cancel: Arc<AtomicBool>,
        control: &Arc<VmControl>,
        attach: F,
    ) -> Result<ExitReason, MachineError>
    where
        F: FnOnce(&DeviceAttach) -> Result<Vec<(u64, u64, Box<dyn MmioDevice>)>, MachineError>,
    {
        config.validate()?;
        let cmdline = guest_cmdline_for(&config.cmdline, config.boot_disk);
        tracing::info!(
            target: "ternvale::boot",
            name = %config.name,
            cpus = config.cpus,
            ram_mib = config.ram_mib,
            boot_disk = config.boot_disk,
            "starting vm"
        );
        let inputs = Inputs::read(config)?;
        let nvram = config.nvram_path()?;
        let ram_size = config.ram_mib << 20;
        let vm = ternvale_hv::Vm::create()?;
        let gic = Arc::new(vm.create_gic(GIC_DIST_BASE, GIC_REDIST_BASE)?);
        let memory = Arc::new(Mutex::new(crate::memory::GuestMemory::new()?));
        let fw_cfg = {
            let mut mem = crate::lockwatch::lock(&memory, "guest-memory");
            mem.map(&vm, RAM_BASE, ram_size)?;
            inputs.map(&mut mem, &vm)?;
            crate::acpi::prepare(&mut mem, &vm, config)?
        };
        let spi_levels = Arc::new(Mutex::new(Vec::new()));
        let attached = DeviceAttach::new(
            Arc::clone(&memory),
            Arc::clone(&gic),
            Arc::clone(&spi_levels),
        );
        let devices = attach(&attached)?;
        // The PCI count includes the host bridge.
        let present = devices.len() + attached.pci.function_count().saturating_sub(1);
        if present < config.disks.len() + config.nics.len() {
            tracing::warn!(
                target: "ternvale::boot",
                disks = config.disks.len(),
                nics = config.nics.len(),
                attached = present,
                "some config disks or nics were not attached through DeviceAttach"
            );
        }
        let dtb = inputs.dtb(&cmdline, config.cpus, ram_size)?;
        control.diagnostics().set_dtb(&dtb);
        let serial: attach::SharedSerial = Arc::new(Mutex::new(serial));
        let irq_level = Arc::new(AtomicBool::new(false));
        attach::install_uart_irq(&serial, Arc::clone(&gic), Arc::clone(&irq_level));
        let redist = RedistMap::new(config.cpus);
        let mut bus = MmioBus::with_trace(Arc::clone(control.diagnostics().mmio()));
        bus.register(
            GIC_REDIST_BASE,
            GIC_REDIST_SIZE,
            Box::new(RedistId::new(Arc::clone(&redist))),
        )?;
        bus.register(
            UART_BASE,
            PL011_REG_SIZE,
            Box::new(attach::SharedUart(Arc::clone(&serial))),
        )?;
        bus.register(RTC_BASE, PL031_REG_SIZE, Box::new(Pl031::new()))?;
        if let Some(path) = &nvram {
            bus.register(
                FLASH_VARS_BASE,
                FLASH_BANK_SIZE,
                Box::new(images::vars_flash(path)?),
            )?;
        }
        for (base, size, device) in devices.into_iter().chain(fw_cfg) {
            bus.register(base, size, device)?;
        }
        attached.pci.register(&mut bus)?;
        // PCI functions (and their guest-memory handles) must die with the bus, before the VM.
        drop(attached);
        let power = Arc::new(CpuPower::new(config.cpus, RAM_BASE, ram_size));
        control.bind(host::control_hooks(&power));
        let shutdown = Arc::new(AtomicBool::new(false));
        let watchdog = Arc::new(Watchdog::new(HANG));
        let stop_watch = Arc::clone(&shutdown);
        let dog = Arc::clone(&watchdog);
        let watch_control = Arc::clone(control);
        let _watch = std::thread::spawn(move || host::watch_loop(dog, stop_watch, watch_control));
        let stdin = {
            let (serial, shutdown, power) = (
                Arc::clone(&serial),
                Arc::clone(&shutdown),
                Arc::clone(&power),
            );
            std::thread::spawn(move || host::stdin_loop(serial, shutdown, power))
        };
        let poll = host::Poll {
            shutdown: Arc::clone(&shutdown),
            power: Arc::clone(&power),
            gic: Arc::clone(&gic),
            irq_level: Arc::clone(&irq_level),
            spi_levels: Arc::clone(&spi_levels),
            cancel: Arc::clone(&cancel),
        };
        let poll = std::thread::spawn(move || host::poll_loop(poll));
        let images: Images<'_> = inputs.images(&dtb, ram_size);
        let vtimer_offset = OnceLock::new();
        let shared = vcpu_thread::Shared {
            name: &config.name,
            vm: &vm,
            gic: &gic,
            memory: &memory,
            bus: &bus,
            power: &power,
            redist: &redist,
            watchdog: &watchdog,
            images: &images,
            vtimer_offset: &vtimer_offset,
            control,
        };
        let outcome = run_vcpus(config.cpus, &shared);
        control.mark_stopping(power.stop_reason().unwrap_or(ExitReason::Canceled));
        shutdown.store(true, Ordering::Release);
        for (what, handle) in [("stdin", stdin), ("poll", poll)] {
            if handle.join().is_err() {
                tracing::error!(target: "ternvale::boot", thread = what, "host thread panicked");
            }
        }
        outcome?;
        let exit = power.stop_reason().unwrap_or(ExitReason::Canceled);
        tracing::info!(target: "ternvale::boot", ?exit, cpus = config.cpus, "vm stopped");
        let locks = crate::lockwatch::LockWatch::global().stats();
        tracing::info!(
            target: "ternvale::lock",
            contended = locks.contended,
            long_waits = locks.long_waits,
            max_wait_us = locks.max_wait_us,
            deadlocks = locks.deadlocks,
            "lock watch summary (process-wide)"
        );
        drop(bus);
        drop(serial);
        drop(memory);
        drop(gic);
        drop(vm);
        Ok(exit)
    }
}

/// One scoped host thread per guest CPU, named `vcpu<N>`. Returns the first
/// thread error after every thread has joined.
fn run_vcpus(cpus: u32, shared: &vcpu_thread::Shared<'_>) -> Result<(), MachineError> {
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        let mut first_error = None;
        for index in 0..cpus {
            let spawned = std::thread::Builder::new()
                .name(format!("vcpu{index}"))
                .spawn_scoped(scope, move || vcpu_thread::vcpu_thread(index, shared));
            match spawned {
                Ok(handle) => handles.push((index, handle)),
                Err(source) => {
                    tracing::error!(target: "ternvale::vcpu", cpu = index, error = %source, "could not spawn vcpu thread");
                    shared.power.request_stop(ExitReason::Canceled);
                    first_error = Some(MachineError::Spawn { cpu: index, source });
                    break;
                }
            }
        }
        tracing::info!(target: "ternvale::boot", threads = handles.len(), "vcpu threads started");
        if first_error.is_none() {
            shared.control.mark_running();
        }
        for (index, handle) in handles {
            let result = handle
                .join()
                .unwrap_or(Err(MachineError::VcpuPanic { cpu: index }));
            if let Err(error) = result {
                tracing::error!(target: "ternvale::vcpu", cpu = index, error = %error, "vcpu thread returned an error");
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    })
}

fn read_file(what: &'static str, path: &std::path::Path) -> Result<Vec<u8>, MachineError> {
    attach::read_boot_file(what, path)
}
