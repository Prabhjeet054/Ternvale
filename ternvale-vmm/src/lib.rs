//! Virtual machine lifecycle, vCPU, and memory orchestration.
//!
//! [`GuestMemory`] allocates host RAM and maps it into the guest.

mod boot;
mod esr;
mod fdt;
mod gic_redist;
mod linux;
mod machine;
mod memory;
mod mmio;
pub mod pci;
mod platform;
mod psci;
mod serial;
mod smp;
mod vcpu;
mod watchdog;

pub use boot::{load as load_payload, stage as stage_payload, BootError, LoadInfo, PAYLOAD_GPA};
pub use esr::{decode as decode_esr, ExitEvent};
pub use fdt::{build_fdt, write_fdt, FdtError, GuestFdt, PL011_REG_SIZE, UART_SPI, VIRTIO_SPI0};
pub use linux::{
    load_linux, parse_header, place, BootRegs, ImageHeader, LinuxBootError, LinuxLayout,
    CPSR_EL1H_MASKED, HEADER_LEN, IMAGE_MAGIC, KERNEL_ALIGN,
};
pub use machine::{
    guest_cmdline, guest_cmdline_for, DeviceAttach, Machine, MachineError, DEFAULT_CMDLINE,
    DISK_ROOT_CMDLINE,
};
pub use memory::{GuestMemory, MemoryError, HOST_PAGE_SIZE};
pub use mmio::{GuestRegs, MmioBus, MmioDevice, MmioError};
pub use platform::{
    Layout, PlatformError, Region, GIC_DIST_BASE, GIC_REDIST_BASE, PCIE_ECAM_BASE, PCIE_ECAM_SIZE,
    PCIE_MMIO_BASE, PCIE_MMIO_SIZE, RAM_BASE, RTC_BASE, UART_BASE, VIRTIO_MMIO_BASE,
    VIRTIO_MMIO_SLOTS, VIRTIO_MMIO_SLOT_SIZE,
};
pub use psci::{call as psci_call, PowerRequest, PsciAction, TRAP_PC_ADVANCE};
pub use serial::SerialDevice;
pub use smp::{dt_cpu_reg, mpidr, CpuPower, MPIDR_AFFINITY_MASK, MPIDR_RES1};
/// The deadlock detector every mutex in this crate and `ternvale-devices` goes through.
pub use ternvale_log::lockwatch;
pub use vcpu::{process_cpu_ms, ExitReason, Vcpu, VcpuError, VcpuStats, VcpuStop};

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
