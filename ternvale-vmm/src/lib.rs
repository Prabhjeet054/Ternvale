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
mod platform;
mod psci;
mod serial;
mod vcpu;
mod watchdog;

pub use boot::{load as load_payload, stage as stage_payload, BootError, LoadInfo, PAYLOAD_GPA};
pub use esr::{decode as decode_esr, ExitEvent};
pub use fdt::{build_fdt, write_fdt, FdtError, GuestFdt, PL011_REG_SIZE, UART_SPI};
pub use linux::{
    load_linux, parse_header, place, BootRegs, ImageHeader, LinuxBootError, LinuxLayout,
    CPSR_EL1H_MASKED, HEADER_LEN, IMAGE_MAGIC, KERNEL_ALIGN,
};
pub use machine::{guest_cmdline, Machine, MachineError, DEFAULT_CMDLINE};
pub use memory::{GuestMemory, MemoryError, HOST_PAGE_SIZE};
pub use mmio::{GuestRegs, MmioBus, MmioDevice, MmioError};
pub use platform::{
    Layout, PlatformError, Region, GIC_DIST_BASE, GIC_REDIST_BASE, PCIE_ECAM_BASE, PCIE_MMIO_BASE,
    RAM_BASE, RTC_BASE, UART_BASE, VIRTIO_MMIO_BASE,
};
pub use psci::{call as psci_call, PsciAction, TRAP_PC_ADVANCE};
pub use serial::SerialDevice;
pub use vcpu::{ExitReason, Vcpu, VcpuError, VcpuStop};

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
