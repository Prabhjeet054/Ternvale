//! Boot the test kernel and match serial output. Built with `--features boot-test`.
//!
//! `scripts/boot-test.sh` runs this binary and keeps the logs under `target/boot-logs/`.
//! Set `TERNVALE_BOOT_SCENARIO=rootfs` for the virtio-blk persistence scenario
//! (requires `./scripts/make-rootfs.sh` assets under `test-assets/virtio-root/`).

mod common;
mod initrd;
mod rootfs;

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
        other => Err(format!(
            "unknown TERNVALE_BOOT_SCENARIO={other:?} (expected initrd or rootfs)"
        )),
    }
}
