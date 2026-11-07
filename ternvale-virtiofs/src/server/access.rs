//! `FUSE_ACCESS`.

use crate::fuse::{self, InHeader};
use crate::server::Server;

const W_OK: u32 = 2;

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn access(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    if server.inodes.get(hdr.nodeid).is_none() {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    }
    let mask = crate::wire::u32_at(body, 0);
    if server.read_only && mask & W_OK != 0 {
        tracing::warn!(
            target: "ternvale::virtiofs",
            reason = "ACCESS W_OK on read-only share",
            "rejected share access"
        );
        return fuse::reply(hdr.unique, libc::EROFS, &[]);
    }
    fuse::reply(hdr.unique, 0, &[])
}
