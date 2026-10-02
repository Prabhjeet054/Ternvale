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
//! shell, open the front page and Boot Manager, and check that an NV variable survives a reboot.
//! Set `TERNVALE_BOOT_SCENARIO=installer` to boot the Alpine arm64 ISO
//! (`./scripts/fetch-installer-iso.sh`) from UEFI on a read-only virtio-blk disk and start
//! `setup-alpine` (`TERNVALE_INSTALLER_TRANSPORT=pci` puts the disk on virtio-pci).

mod agent;
mod common;
mod devices;
mod firmware;
mod idle;
mod initrd;
mod installer;
mod net;
mod pci;
mod rootfs;
mod smp;

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
        "installer" => installer::run(),
        other => Err(format!(
            "unknown TERNVALE_BOOT_SCENARIO={other:?} (expected initrd, rootfs, pci, net, devices, agent, smp, firmware, or installer)"
        )),
    }
}
