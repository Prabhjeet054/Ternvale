//! Virtual machine lifecycle, vCPU, and memory orchestration.
//!
//! [`GuestMemory`] allocates host RAM and maps it into the guest.

mod acpi;
mod acpi_check;
mod boot;
mod boot_order;
mod control;
mod cpu_profile;
pub mod crash;
mod diag;
mod esr;
mod fdt;
mod firmware;
mod fw_cfg;
mod gic_redist;
mod guest_fault;
mod linux;
mod machine;
mod memory;
mod mmio;
mod nested;
pub mod pci;
mod platform;
mod pmu;
mod power;
mod psci;
mod pvpanic;
mod rtc;
mod sbsa_gwdt;
mod serial;
mod smp;
mod supervisor;
mod tpm;
mod usb;
mod vcpu;
mod watchdog;

pub use acpi::{build_acpi_offline, dump_guest_acpi};
pub use acpi_check::acpi_config;
pub use boot::{load as load_payload, stage as stage_payload, BootError, LoadInfo, PAYLOAD_GPA};
pub use boot_order::{disk_ofw_path, pci_disk_bdf, BOOTORDER_FILE};
pub use control::{
    ControlError, ControlHooks, CpuStats, StopCause, VmControl, VmState, VmStats, VmStatus,
};
pub use cpu_profile::{
    apply_and_capture, evaluate_restore, format_mismatch_error, from_hex_map, host_fingerprint,
    mismatches, to_hex_map, write_cpu_json, CpuFingerprint, CpuMismatch,
};
pub use diag::{DeviceCount, Diagnostics, MmioEvent, MmioTrace, MMIO_TRACE_LEN};
pub use esr::{decode as decode_esr, ExitEvent};
pub use fdt::{
    build_fdt, write_fdt, FdtError, GuestFdt, GED_SPI, GPIO_SPI, PL011_REG_SIZE, PL061_REG_SIZE,
    TIMER_ALWAYS_ON, TIMER_PPIS, UART_SPI, VIRTIO_SPI0, WATCHDOG_SPI,
};
pub use firmware::{FirmwareError, RomdWindow, VarsFlash};
pub use fw_cfg::{FwCfg, FW_CFG_REG_SIZE};
pub use linux::{
    boot_cpsr, load_linux, parse_header, place, BootRegs, ImageHeader, LinuxBootError, LinuxLayout,
    CPSR_EL1H_MASKED, CPSR_EL2H_MASKED, HEADER_LEN, IMAGE_MAGIC, KERNEL_ALIGN,
};
pub use machine::cmdline::resolve_cmdline;
pub use machine::{
    guest_cmdline, guest_cmdline_for, DeviceAttach, Machine, MachineError, DEFAULT_CMDLINE,
    DISK_ROOT_CMDLINE,
};
pub use memory::{GuestMemory, MemAdvice, MemoryError, HOST_PAGE_SIZE};
pub use mmio::{GuestRegs, KickPort, MmioBus, MmioDevice, MmioError};
pub use platform::{
    Layout, PlatformError, Region, ACPI_BASE, ACPI_SIZE, FLASH_BANK_SIZE, FLASH_CODE_BASE,
    FLASH_VARS_BASE, FW_CFG_BASE, GED_BASE, GIC_DIST_BASE, GIC_REDIST_BASE, PCIE_ECAM_BASE,
    PCIE_ECAM_SIZE, PCIE_MMIO_BASE, PCIE_MMIO_SIZE, PL061_BASE, PVPANIC_BASE, PVPANIC_SIZE,
    RAM_BASE, RTC_BASE, SBSA_GWDT_CONTROL_BASE, SBSA_GWDT_CONTROL_SIZE, SBSA_GWDT_REFRESH_BASE,
    SBSA_GWDT_REFRESH_SIZE, TPM_CRB_BASE, TPM_CRB_SIZE, UART_BASE, VIRTIO_MMIO_BASE,
    VIRTIO_MMIO_SLOTS, VIRTIO_MMIO_SLOT_SIZE,
};
pub use power::{GedRegs, Pl061, PowerButton, GED_REG_SIZE, KEY_POWER, POWER_GPIO_PIN};
pub use psci::{call as psci_call, PowerRequest, PsciAction, TRAP_PC_ADVANCE};
pub use rtc::{Pl031, PL031_REG_SIZE, RTC_SPI};
pub use sbsa_gwdt::{SbsaWatchdog, SBSA_FRAME, WCS_EN, WCS_WS0, WCS_WS1};
pub use serial::SerialDevice;
pub use smp::{dt_cpu_reg, mpidr, CpuPower, MPIDR_AFFINITY_MASK, MPIDR_RES1};
pub use supervisor::{SuperviseDecision, VmmSupervisor, CRASH_WINDOW, MAX_CRASHES};
/// Host EL2 for `nested_virt` (Hypervisor.framework probe).
pub use ternvale_hv::host_supports_el2;
/// The deadlock detector every mutex in this crate and `ternvale-devices` goes through.
pub use ternvale_log::lockwatch;
pub use tpm::TPM_DEVICE_NAME;
pub use usb::{USB_SPI, XHCI_DEVICE_NAME};
pub use vcpu::{process_cpu_ms, ExitReason, Vcpu, VcpuError, VcpuStats, VcpuStatsSource, VcpuStop};

#[cfg(test)]
#[path = "gic_hv_test.rs"]
mod gic_hv_test;
#[cfg(test)]
#[path = "pmu_hv_test.rs"]
mod pmu_hv_test;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-vmm");
    }
}
