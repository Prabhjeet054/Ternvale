//! Virtual machine lifecycle, vCPU, and memory orchestration.
//!
//! [`GuestMemory`] allocates host RAM and maps it into the guest.

mod memory;
mod vcpu;

pub use memory::{GuestMemory, MemoryError, HOST_PAGE_SIZE};
pub use vcpu::{ExitReason, Vcpu, VcpuError, VcpuStop};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-vmm");
    }
}
