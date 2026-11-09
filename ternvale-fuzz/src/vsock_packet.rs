//! virtio-vsock header parser.

use ternvale_devices::VsockHeader;

use crate::quiet;

/// Decode a guest TX packet header.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let _ = VsockHeader::parse(data);
}
