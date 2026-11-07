//! LOOKUP and FORGET.

use crate::fuse::{self, c_string, InHeader};
use crate::inode;
use crate::server::Server;
use crate::sys;

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(crate) fn lookup(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    let Some(raw) = c_string(body) else {
        tracing::warn!(
            target: "ternvale::virtiofs",
            reason = "LOOKUP name is not UTF-8",
            "rejected share lookup"
        );
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    };
    let name = match server.child_name(hdr.nodeid, raw, false) {
        Ok(name) => name,
        Err(errno) => return fuse::reply(hdr.unique, errno, &[]),
    };
    let Some(parent) = server.inodes.get(hdr.nodeid) else {
        tracing::warn!(
            target: "ternvale::virtiofs",
            nodeid = hdr.nodeid,
            reason = "LOOKUP parent missing",
            "rejected share lookup"
        );
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    let parent_fd = parent.raw();
    let child = match sys::openat_nofollow(parent_fd, &name, libc::O_RDONLY) {
        Ok(fd) => fd,
        Err(error) => {
            tracing::warn!(
                target: "ternvale::virtiofs",
                name = %name,
                error = %error,
                reason = "openat failed",
                "rejected share lookup"
            );
            return fuse::reply(hdr.unique, sys::errno(&error), &[]);
        }
    };
    let st = match sys::fstat(sys::raw(&child)) {
        Ok(st) => st,
        Err(error) => return fuse::reply(hdr.unique, sys::errno(&error), &[]),
    };
    let mode = u32::from(st.st_mode);
    match server.inodes.insert(child, mode) {
        Ok((nodeid, generation)) => {
            let inode = match server.inodes.get(nodeid) {
                Some(inode) => inode,
                None => return fuse::reply(hdr.unique, libc::ENOENT, &[]),
            };
            let attr = match inode::attr_of(inode) {
                Ok(attr) => attr,
                Err(errno) => return fuse::reply(hdr.unique, errno, &[]),
            };
            fuse::reply(
                hdr.unique,
                0,
                &fuse::encode_entry(nodeid, generation, &attr),
            )
        }
        Err(errno) => fuse::reply(hdr.unique, errno, &[]),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn forget(server: &mut Server, hdr: InHeader, body: &[u8]) {
    let nlookup = if body.len() >= 8 {
        crate::wire::u64_at(body, 0)
    } else {
        1
    };
    server.inodes.forget(hdr.nodeid, nlookup);
}
