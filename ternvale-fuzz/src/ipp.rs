//! IPP/HTTP request inspector (guest-controlled).

use crate::quiet;

/// Feed a byte slice into the host IPP proxy classifier.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let _ = ternvale_devices::inspect_ipp(data);
}
