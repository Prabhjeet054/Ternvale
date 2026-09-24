//! Virtual machine lifecycle, vCPU, and memory orchestration.
//!
//! [`GuestMemory`] allocates host RAM and maps it into the guest.

mod boot;
mod esr;
mod linux;
mod memory;
mod mmio;
mod platform;
mod vcpu;

pub use boot::{load as load_payload, stage as stage_payload, BootError, LoadInfo, PAYLOAD_GPA};
pub use esr::{decode as decode_esr, ExitEvent};
pub use linux::{
    load_linux, parse_header, place, BootRegs, ImageHeader, LinuxBootError, LinuxLayout,
    CPSR_EL1H_MASKED, HEADER_LEN, IMAGE_MAGIC, KERNEL_ALIGN,
};
pub use memory::{GuestMemory, MemoryError, HOST_PAGE_SIZE};
pub use mmio::{GuestRegs, MmioBus, MmioDevice, MmioError};
pub use platform::{
    Layout, PlatformError, Region, GIC_DIST_BASE, GIC_REDIST_BASE, PCIE_ECAM_BASE, PCIE_MMIO_BASE,
    RAM_BASE, RTC_BASE, UART_BASE, VIRTIO_MMIO_BASE,
};
pub use vcpu::{ExitReason, Vcpu, VcpuError, VcpuStop};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-vmm");
    }
}
