//! Fuzz harnesses for guest-controlled parsers and device request paths.
//!
//! Each `run` entry point is also a cargo-fuzz target under `fuzz/`. Inputs
//! go through the same MMIO or virtqueue interface a guest uses, against
//! fake [`ternvale_vmm::GuestMemory`] (no Hypervisor.framework).

mod agent;
mod crypto;
mod esr;
mod fuse;
mod guest;
mod psci;
mod tpm;
mod transport;
mod user_net;
mod virtio_balloon;
mod virtio_blk;
mod virtio_fs;
mod virtio_gpu;
mod virtio_input;
mod virtio_net;
mod virtio_pci;
mod virtio_rng;
mod virtio_vsock;
mod virtqueue;
mod vsock_packet;
mod xhci;

pub use agent::run as agent_proto;
pub use crypto::run as crypto_at_rest;
pub use esr::run as esr;
pub use fuse::run as fuse;
pub use psci::run as psci;
pub use tpm::run as tpm_crb;
pub use user_net::run as user_net;
pub use virtio_balloon::run as virtio_balloon;
pub use virtio_blk::run as virtio_blk;
pub use virtio_fs::run as virtio_fs;
pub use virtio_gpu::run as virtio_gpu;
pub use virtio_input::run as virtio_input;
pub use virtio_net::run as virtio_net;
pub use virtio_pci::run as virtio_pci;
pub use virtio_rng::run as virtio_rng;
pub use virtio_vsock::run as virtio_vsock;
pub use virtqueue::run as virtqueue;
pub use vsock_packet::run as vsock_packet;
pub use xhci::run as xhci_trb;

/// Install a no-op tracing subscriber so fuzz iterations do not format logs.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all)]
pub fn quiet() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(tracing::subscriber::NoSubscriber::new());
    });
}

/// Names of cargo-fuzz targets, matching `fuzz/fuzz_targets/*.rs`.
pub const TARGETS: &[&str] = &[
    "virtqueue",
    "virtio_blk",
    "virtio_net",
    "virtio_rng",
    "virtio_vsock",
    "virtio_gpu",
    "virtio_input",
    "virtio_balloon",
    "virtio_fs",
    "virtio_pci",
    "xhci_trb",
    "tpm_crb",
    "vsock_packet",
    "agent_proto",
    "esr",
    "psci",
    "user_net",
    "fuse",
    "crypto",
];

/// Run the named target on `data`. Unknown names are ignored.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(target))]
pub fn run_named(target: &str, data: &[u8]) {
    match target {
        "virtqueue" => virtqueue(data),
        "virtio_blk" => virtio_blk(data),
        "virtio_net" => virtio_net(data),
        "virtio_rng" => virtio_rng(data),
        "virtio_vsock" => virtio_vsock(data),
        "virtio_gpu" => virtio_gpu(data),
        "virtio_input" => virtio_input(data),
        "virtio_balloon" => virtio_balloon(data),
        "virtio_fs" => virtio_fs(data),
        "virtio_pci" => virtio_pci(data),
        "xhci_trb" => xhci_trb(data),
        "tpm_crb" => tpm_crb(data),
        "vsock_packet" => vsock_packet(data),
        "agent_proto" => agent_proto(data),
        "esr" => esr(data),
        "psci" => psci(data),
        "user_net" => user_net(data),
        "fuse" => fuse(data),
        "crypto" => crypto_at_rest(data),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-fuzz");
    }

    #[test]
    fn seed_corpus_does_not_panic() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus");
        for target in TARGETS {
            let dir = root.join(target);
            let entries = fs::read_dir(&dir).unwrap_or_else(|error| {
                panic!("corpus {dir:?}: {error}");
            });
            let mut n = 0;
            for entry in entries {
                let path = entry.expect("entry").path();
                if path.is_file() {
                    let data = fs::read(&path).expect("seed");
                    run_named(target, &data);
                    n += 1;
                }
            }
            assert!(n > 0, "no seeds in {dir:?}");
        }
    }

    #[test]
    fn regression_inputs_do_not_panic() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/regressions");
        if !root.exists() {
            return;
        }
        for target in TARGETS {
            let dir = root.join(target);
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries {
                let path = entry.expect("entry").path();
                if path.is_file() && path.file_name().is_some_and(|n| n != ".gitkeep") {
                    let data = fs::read(&path).expect("regression");
                    run_named(target, &data);
                }
            }
        }
    }
}
