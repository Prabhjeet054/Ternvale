//! Venus / virtio-gpu SUBMIT_3D decoder (no hypervisor).

use ternvale_gpu3d_host::fuzz_decode;

use crate::quiet;

/// Decode untrusted command-stream bytes.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    fuzz_decode(data);
}
