//! Boot the test kernel and match serial output. Built with `--features boot-test`.
//!
//! `scripts/boot-test.sh` runs this binary and keeps the logs under `target/boot-logs/`.
//! Set `TERNVALE_BOOT_SCENARIO=rootfs` for the virtio-blk persistence scenario
//! (requires `./scripts/make-rootfs.sh` assets under `test-assets/virtio-root/`).
//! Set `TERNVALE_BOOT_SCENARIO=pci` for the same scenario with the root disk on virtio-pci
//! behind the ECAM host bridge, plus `lspci -nn` and a hand mount of a second virtio-pci
//! disk (also needs `./scripts/make-pci-assets.sh`; the kernel has `virtio_pci` built in).
//! Set `TERNVALE_BOOT_SCENARIO=net` to ping the loopback gateway over virtio-net
//! (requires `./scripts/make-net-initramfs.sh` assets under `test-assets/virtio-net/`).
//! Set `TERNVALE_BOOT_SCENARIO=devices` to read /dev/hwrng and ping/pong over vsock
//! (requires `./scripts/make-devices-initramfs.sh` assets under `test-assets/virtio-devices/`).
//! Set `TERNVALE_BOOT_SCENARIO=agent` to run the guest agent from the same assets against the
//! host `AgentServer`: handshake, heartbeats, reconnect after a server restart, requests, and
//! a guest power-off on `Shutdown`.
//! Set `TERNVALE_BOOT_SCENARIO=smp` to check `nproc`, `/proc/cpuinfo`, the host CPU cost of
//! an idle guest, and a per-CPU `dd` workload on `TERNVALE_BOOT_CPUS` CPUs (default 4) with
//! the initrd assets.
//! Set `TERNVALE_BOOT_SCENARIO=firmware` to boot EDK2 (`./scripts/fetch-firmware.sh`) to the UEFI
//! shell, open the front page and Boot Manager, and check that an NV variable survives a reboot
//! (with `firmware_tables = "fdt"`, so `dmem` must show a DTB and no ACPI table).
//! Set `TERNVALE_BOOT_SCENARIO=firmware-acpi` to boot EDK2 with the default `acpi` tables and
//! walk the ACPI tables EDK2 installed from fw_cfg with `dmem`, comparing them with the builder.
//! Set `TERNVALE_BOOT_SCENARIO=firmware-debug` to boot a verbose debug EDK2
//! (`./scripts/fetch-debug-firmware.sh`) with `acpi` and check EDK2's own log for the ACPI install.
//! Set `TERNVALE_BOOT_SCENARIO=uefi-linux` to boot the test kernel and initrd through UEFI
//! (`firmware_tables = "fdt"`) from a FAT16 disk and run the initrd checks.
//! Set `TERNVALE_BOOT_SCENARIO=uefi-linux-acpi` to boot the same kernel with `acpi` from a
//! virtio-pci disk and check it finds `\_SB.PCI0` and the disk by PCI enumeration alone.
//! Set `TERNVALE_BOOT_SCENARIO=uefi-lspci` to boot the rootfs kernel twice through UEFI with
//! the same virtio-pci disks, `fdt` then `acpi`, and compare `lspci -nnk` (needs
//! `./scripts/make-rootfs.sh` and `./scripts/make-pci-assets.sh`).
//! `TERNVALE_FIRMWARE` replaces the EDK2 image in every UEFI scenario.
//! Set `TERNVALE_BOOT_SCENARIO=installer` to boot the Alpine arm64 ISO
//! (`./scripts/fetch-installer-iso.sh`) from UEFI on a read-only virtio-blk disk and start
//! `setup-alpine` (`TERNVALE_INSTALLER_TRANSPORT=pci` puts the disk on virtio-pci).

mod agent;
mod common;
mod cpio;
mod devices;
mod fat;
mod firmware;
mod firmware_acpi;
mod firmware_debug;
mod idle;
mod initrd;
mod installer;
mod net;
mod pci;
mod rootfs;
mod smp;
mod uefi_dmem;
mod uefi_linux;
mod uefi_linux_acpi;
mod uefi_lspci;

fn main() {
    let code = match run() {
        Ok(()) => 0,
        Err(error) => {
            tracing::error!(target: "ternvale::boot", error = %error, "boot harness failed");
            eprintln!("boot harness failed: {error}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<(), String> {
    let scenario = std::env::var("TERNVALE_BOOT_SCENARIO").unwrap_or_else(|_| "initrd".to_string());
    tracing::info!(target: "ternvale::boot", scenario = %scenario, "boot harness scenario");
    match scenario.as_str() {
        "initrd" | "" => initrd::run(),
        "rootfs" => rootfs::run(),
        "pci" => rootfs::run_pci(),
        "net" => net::run(),
        "devices" => devices::run(),
        "agent" => agent::run(),
        "smp" => smp::run(),
        "firmware" => firmware::run(),
        "firmware-acpi" => firmware_acpi::run(),
        "firmware-debug" => firmware_debug::run(),
        "uefi-linux" => uefi_linux::run(),
        "uefi-linux-acpi" => uefi_linux_acpi::run(),
        "uefi-lspci" => uefi_lspci::run(),
        "installer" => installer::run(),
        other => Err(format!(
            "unknown TERNVALE_BOOT_SCENARIO={other:?} (expected initrd, rootfs, pci, net, devices, agent, smp, firmware, firmware-acpi, firmware-debug, uefi-linux, uefi-linux-acpi, uefi-lspci, or installer)"
        )),
    }
}
