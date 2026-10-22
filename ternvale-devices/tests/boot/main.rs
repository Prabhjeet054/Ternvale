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
//! Set `TERNVALE_BOOT_SCENARIO=snapshot` for a two-boot layered disk test (needs
//! `./scripts/make-snapshot-initramfs.sh` under `test-assets/snapshot-boot/`).
//! Set `TERNVALE_BOOT_SCENARIO=audio` to load virtio_snd and play a 440 Hz sine through
//! speaker-test (needs `./scripts/make-audio-initramfs.sh` under `test-assets/virtio-audio/`).
//! Set `TERNVALE_BOOT_SCENARIO=devices` to read /dev/hwrng and ping/pong over vsock
//! (requires `./scripts/make-devices-initramfs.sh` assets under `test-assets/virtio-devices/`).
//! Set `TERNVALE_BOOT_SCENARIO=gpu` to boot Linux's virtio-gpu driver on virtio-mmio with a
//! 1024x768 scanout and check fb0's sysfs geometry, `fbset`, and two rows written through
//! /dev/fb0 in the host frame (requires `./scripts/make-gpu-initramfs.sh` assets under
//! `test-assets/virtio-gpu/`).
//! Set `TERNVALE_BOOT_SCENARIO=input` to add a virtio-input keyboard and tablet to that VM,
//! check the guest's evdev view of both, and inject key, pointer, button and wheel events
//! from the host that the guest must read back unchanged (same assets).
//! Set `TERNVALE_BOOT_SCENARIO=getty` to boot the same assets with `ternvale.getty`: a getty on
//! the framebuffer console must take a login name and a shell command typed on the virtio-input
//! keyboard (macOS key codes through the viewer keymap); both report on the serial console.
//! Set `TERNVALE_BOOT_SCENARIO=desktop` to boot the Xorg + twm desktop image from
//! `scripts/make-desktop-rootfs.sh` (virtio-blk root, virtio-gpu, virtio-input keyboard and
//! tablet): a command typed into its xterm must run, tablet moves must put the X pointer on
//! the same pixels, and a click must reach the xterm.
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
//! Set `TERNVALE_BOOT_SCENARIO=firmware-tpm` to boot EDK2 twice with `[tpm] backend = "swtpm"`
//! (needs `brew install swtpm`) and check the TPM identity survives the restart (docs/TPM.md).
//! Set `TERNVALE_BOOT_SCENARIO=firmware-debug` to boot a verbose debug EDK2
//! (`./scripts/fetch-debug-firmware.sh`) with `acpi` and check EDK2's own log for the ACPI install.
//! Set `TERNVALE_BOOT_SCENARIO=uefi-linux` to boot the test kernel and initrd through UEFI
//! (`firmware_tables = "fdt"`) from a FAT16 disk and run the initrd checks.
//! Set `TERNVALE_BOOT_SCENARIO=uefi-linux-acpi` to boot the same kernel with `acpi` from a
//! virtio-pci disk and check it finds `\_SB.PCI0` and the disk by PCI enumeration alone.
//! Set `TERNVALE_BOOT_SCENARIO=uefi-lspci` to boot the rootfs kernel twice through UEFI with
//! the same virtio-pci disks, `fdt` then `acpi`, and compare `lspci -nnk` (needs
//! `./scripts/make-rootfs.sh` and `./scripts/make-pci-assets.sh`).
//! Set `TERNVALE_BOOT_SCENARIO=uefi-linux-tpm` to boot Linux through EDK2 with `acpi` and
//! swtpm twice, run tpm2-tools in the guest, and check random bytes, the EK and an NV index
//! across the restart (needs swtpm and `./scripts/make-tpm-assets.sh`; docs/TPM.md).
//! `TERNVALE_FIRMWARE` replaces the EDK2 image in every UEFI scenario.
//! Set `TERNVALE_BOOT_SCENARIO=installer` to boot the Alpine arm64 ISO
//! (`./scripts/fetch-installer-iso.sh`) from UEFI on a read-only virtio-blk disk and start
//! `setup-alpine` (`TERNVALE_INSTALLER_TRANSPORT=pci` puts the disk on virtio-pci).
//! Set `TERNVALE_BOOT_SCENARIO=windows-profile` to boot a blank system disk plus that ISO on
//! virtio-pci twice, without and with `profile = "windows"`, and check EDK2 loads the ISO first
//! only when the fw_cfg `bootorder` says so.
//! Set `TERNVALE_BOOT_SCENARIO=windows-media` to attach the `[iso]` installer (that ISO) and
//! driver (virtio-win, `./scripts/fetch-virtio-win.sh`) slots as read-only optical virtio-pci
//! discs: debug EDK2 must enumerate the IDs the host logged and boot the installer, and a Linux
//! guest must see both as read-only 2048-byte-block discs with the host's bytes.
//! Set `TERNVALE_BOOT_SCENARIO=windows-installer` to boot the Windows 11 ARM64 ISO
//! (`./scripts/fetch-windows-iso.sh` or `TERNVALE_WINDOWS_ISO`) with virtio-win and no
//! disk: EDK2 must load the installer first, its loader must print "Press any key to boot
//! from CD or DVD" (the harness presses Enter), and the boot manager must read `boot.wim`.

mod agent;
mod audio;
mod common;
mod cpio;
mod desktop;
mod devices;
mod fat;
mod firmware;
mod firmware_acpi;
mod firmware_debug;
mod firmware_tpm;
mod getty;
mod gpu;
mod idle;
mod initrd;
mod input;
mod installer;
mod net;
mod pci;
mod rootfs;
mod smp;
mod snapshot;
mod tpm_guest_checks;
mod uefi_dmem;
mod uefi_linux;
mod uefi_linux_acpi;
mod uefi_linux_tpm;
mod uefi_lspci;
mod usb;
mod vm_snapshot;
mod windows_installer;
mod windows_media;
mod windows_profile;

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
        "audio" => audio::run(),
        "snapshot" => snapshot::run(),
        "vm-snapshot" => vm_snapshot::run(),
        "usb" => usb::run(),
        "gpu" => gpu::run(),
        "input" => input::run(),
        "getty" => getty::run(),
        "desktop" => desktop::run(),
        "agent" => agent::run(),
        "smp" => smp::run(),
        "firmware" => firmware::run(),
        "firmware-acpi" => firmware_acpi::run(),
        "firmware-debug" => firmware_debug::run(),
        "firmware-tpm" => firmware_tpm::run(),
        "uefi-linux" => uefi_linux::run(),
        "uefi-linux-acpi" => uefi_linux_acpi::run(),
        "uefi-lspci" => uefi_lspci::run(),
        "uefi-linux-tpm" => uefi_linux_tpm::run(),
        "installer" => installer::run(),
        "windows-profile" => windows_profile::run(),
        "windows-media" => windows_media::run(),
        "windows-installer" => windows_installer::run(),
        other => Err(format!(
            "unknown TERNVALE_BOOT_SCENARIO={other:?} (expected initrd, rootfs, pci, net, devices, audio, snapshot, vm-snapshot, usb, gpu, input, getty, desktop, agent, smp, firmware, firmware-acpi, firmware-debug, firmware-tpm, uefi-linux, uefi-linux-acpi, uefi-lspci, uefi-linux-tpm, installer, windows-profile, windows-media, or windows-installer)"
        )),
    }
}
