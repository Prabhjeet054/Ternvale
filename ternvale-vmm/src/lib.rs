//! Virtual machine lifecycle, vCPU, and memory orchestration.
//!
//! [`GuestMemory`] allocates host RAM and maps it into the guest.

mod boot;
mod esr;
mod memory;
mod mmio;
mod vcpu;

pub use boot::{load as load_payload, stage as stage_payload, BootError, LoadInfo, PAYLOAD_GPA};
pub use esr::{decode as decode_esr, ExitEvent};
pub use memory::{GuestMemory, MemoryError, HOST_PAGE_SIZE};
pub use mmio::{GuestRegs, MmioBus, MmioDevice, MmioError};
pub use vcpu::{ExitReason, Vcpu, VcpuError, VcpuStop};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-vmm");
    }
}
