//! `FUSE_INIT`.

use crate::fuse::{self, InHeader};
use crate::server::Server;

const INIT_IN: usize = 16;
/// `FUSE_ASYNC_READ | FUSE_BIG_WRITES | FUSE_DONT_MASK | FUSE_ATOMIC_O_TRUNC`.
const FLAGS: u32 = 1 | (1 << 5) | (1 << 6) | (1 << 3);
const MAX_WRITE: u32 = 1024 * 1024;

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn init(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    if body.len() < INIT_IN {
        tracing::warn!(
            target: "ternvale::virtiofs",
            reason = "short FUSE_INIT",
            "rejected share request"
        );
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    }
    let major = crate::wire::u32_at(body, 0);
    let minor = crate::wire::u32_at(body, 4);
    tracing::debug!(
        target: "ternvale::virtiofs",
        guest_major = major,
        guest_minor = minor,
        "FUSE_INIT"
    );
    server.inited = true;
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&fuse::KERNEL_MAJOR.to_le_bytes());
    out.extend_from_slice(&fuse::KERNEL_MINOR.to_le_bytes());
    out.extend_from_slice(&MAX_WRITE.to_le_bytes()); // max_readahead
    out.extend_from_slice(&FLAGS.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // max_background
    out.extend_from_slice(&0u16.to_le_bytes()); // congestion
    out.extend_from_slice(&MAX_WRITE.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // time_gran ns
    out.resize(64, 0);
    fuse::reply(hdr.unique, 0, &out)
}
