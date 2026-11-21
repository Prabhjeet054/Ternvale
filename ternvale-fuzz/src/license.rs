//! Hostile `TVLK` blobs must not panic the license parser.

use ternvale_license::blob::fuzz_verify;

/// cargo-fuzz entry (`license`).
#[tracing::instrument(level = "trace", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    crate::quiet();
    fuzz_verify(data);
}
