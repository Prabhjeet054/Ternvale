//! Boot the test kernel and match serial output. Built with `--features boot-test`.
//!
//! `scripts/boot-test.sh` runs this binary and keeps the logs under `target/boot-logs/`.
//! Set `TERNVALE_BOOT_SCENARIO=rootfs` for the virtio-blk persistence scenario
//! (requires `./scripts/make-rootfs.sh` assets under `test-assets/virtio-root/`).
//! Set `TERNVALE_BOOT_SCENARIO=net` to ping the loopback gateway over virtio-net
//! (requires `./scripts/make-net-initramfs.sh` assets under `test-assets/virtio-net/`).
//! Set `TERNVALE_BOOT_SCENARIO=devices` to read /dev/hwrng and ping/pong over vsock
//! (requires `./scripts/make-devices-initramfs.sh` assets under `test-assets/virtio-devices/`).
//! Set `TERNVALE_BOOT_SCENARIO=smp` to check `nproc`, `/proc/cpuinfo`, and a per-CPU `dd`
//! workload on `TERNVALE_BOOT_CPUS` CPUs (default 4) with the initrd assets.

mod common;
mod devices;
mod initrd;
mod net;
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
        "net" => net::run(),
        "devices" => devices::run(),
        "smp" => smp::run(),
        other => Err(format!(
            "unknown TERNVALE_BOOT_SCENARIO={other:?} (expected initrd, rootfs, net, devices, or smp)"
        )),
    }
}
