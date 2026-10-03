//! Virtual machine lifecycle, vCPU, and memory orchestration.
//!
//! [`GuestMemory`] allocates host RAM and maps it into the guest.

mod acpi;
mod acpi_check;
mod boot;
mod control;
mod diag;
mod esr;
mod fdt;
mod firmware;
mod gic_redist;
mod linux;
mod machine;
mod memory;
mod mmio;
pub mod pci;
mod platform;
mod psci;
mod rtc;
mod serial;
mod smp;
mod vcpu;
mod watchdog;

pub use acpi::dump_guest_acpi;
pub use acpi_check::acpi_config;
pub use boot::{load as load_payload, stage as stage_payload, BootError, LoadInfo, PAYLOAD_GPA};
pub use control::{
    ControlError, ControlHooks, CpuStats, StopCause, VmControl, VmState, VmStats, VmStatus,
};
pub use diag::{DeviceCount, Diagnostics, MmioEvent, MmioTrace, MMIO_TRACE_LEN};
pub use esr::{decode as decode_esr, ExitEvent};
pub use fdt::{
    build_fdt, write_fdt, FdtError, GuestFdt, PL011_REG_SIZE, TIMER_ALWAYS_ON, TIMER_PPIS,
    UART_SPI, VIRTIO_SPI0,
};
pub use firmware::{FirmwareError, RomdWindow, VarsFlash};
pub use linux::{
    load_linux, parse_header, place, BootRegs, ImageHeader, LinuxBootError, LinuxLayout,
    CPSR_EL1H_MASKED, HEADER_LEN, IMAGE_MAGIC, KERNEL_ALIGN,
};
pub use machine::cmdline::resolve_cmdline;
pub use machine::{
    guest_cmdline, guest_cmdline_for, DeviceAttach, Machine, MachineError, DEFAULT_CMDLINE,
    DISK_ROOT_CMDLINE,
};
pub use memory::{GuestMemory, MemoryError, HOST_PAGE_SIZE};
pub use mmio::{GuestRegs, MmioBus, MmioDevice, MmioError};
pub use platform::{
    Layout, PlatformError, Region, ACPI_BASE, ACPI_SIZE, FLASH_BANK_SIZE, FLASH_CODE_BASE,
    FLASH_VARS_BASE, GIC_DIST_BASE, GIC_REDIST_BASE, PCIE_ECAM_BASE, PCIE_ECAM_SIZE,
    PCIE_MMIO_BASE, PCIE_MMIO_SIZE, RAM_BASE, RTC_BASE, UART_BASE, VIRTIO_MMIO_BASE,
    VIRTIO_MMIO_SLOTS, VIRTIO_MMIO_SLOT_SIZE,
};
pub use psci::{call as psci_call, PowerRequest, PsciAction, TRAP_PC_ADVANCE};
pub use rtc::{Pl031, PL031_REG_SIZE, RTC_SPI};
pub use serial::SerialDevice;
pub use smp::{dt_cpu_reg, mpidr, CpuPower, MPIDR_AFFINITY_MASK, MPIDR_RES1};
/// The deadlock detector every mutex in this crate and `ternvale-devices` goes through.
pub use ternvale_log::lockwatch;
pub use vcpu::{process_cpu_ms, ExitReason, Vcpu, VcpuError, VcpuStats, VcpuStatsSource, VcpuStop};

#[cfg(test)]
#[path = "gic_hv_test.rs"]
mod gic_hv_test;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-vmm");
    }
}
