//! GETATTR, SETATTR, STATFS.

use crate::fuse::{self, InHeader};
use crate::inode;
use crate::server::Server;
use crate::sys;

const FATTR_MODE: u32 = 1 << 0;
const FATTR_SIZE: u32 = 1 << 3;
const FATTR_FH: u32 = 1 << 6;

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn getattr(server: &mut Server, hdr: InHeader, _body: &[u8]) -> Vec<u8> {
    let Some(inode) = server.inodes.get(hdr.nodeid) else {
        tracing::warn!(
            target: "ternvale::virtiofs",
            nodeid = hdr.nodeid,
            reason = "GETATTR unknown inode",
            "rejected share getattr"
        );
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    match inode::attr_of(inode) {
        Ok(attr) => {
            let mut payload = Vec::with_capacity(16 + fuse::ATTR_SIZE);
            payload.extend_from_slice(&1u64.to_le_bytes()); // attr_valid
            payload.extend_from_slice(&0u32.to_le_bytes());
            payload.extend_from_slice(&0u32.to_le_bytes());
            payload.extend_from_slice(&fuse::encode_attr(&attr));
            fuse::reply(hdr.unique, 0, &payload)
        }
        Err(errno) => fuse::reply(hdr.unique, errno, &[]),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn setattr(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    if let Err(reply) = server.want_write(hdr.unique) {
        return reply;
    }
    if body.len() < 16 {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    }
    let valid = crate::wire::u32_at(body, 0);
    let fh = crate::wire::u64_at(body, 8);
    let size = crate::wire::u64_at(body, 16);
    let mode = crate::wire::u32_at(body, 80);
    let fd = if valid & FATTR_FH != 0 {
        match server.handles.get(fh) {
            Some(h) => crate::handle::Handles::raw(h),
            None => return fuse::reply(hdr.unique, libc::EBADF, &[]),
        }
    } else {
        match server.inodes.get(hdr.nodeid) {
            Some(inode) => inode.raw(),
            None => return fuse::reply(hdr.unique, libc::ENOENT, &[]),
        }
    };
    if valid & FATTR_SIZE != 0 {
        if let Err(error) = sys::ftruncate(fd, size) {
            return fuse::reply(hdr.unique, sys::errno(&error), &[]);
        }
    }
    if valid & FATTR_MODE != 0 {
        if let Err(error) = sys::fchmod(fd, mode & 0o7777) {
            return fuse::reply(hdr.unique, sys::errno(&error), &[]);
        }
    }
    // Guest uid/gid are ignored; attrs always report the host user.
    getattr(server, hdr, &[])
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn statfs(server: &mut Server, hdr: InHeader) -> Vec<u8> {
    let Some(root) = server.inodes.get(inode::ROOT_ID) else {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    match sys::fstatfs(root.raw()) {
        Ok(st) => {
            // fuse_kstatfs: 80 bytes.
            let mut out = vec![0u8; 80];
            out[0..8].copy_from_slice(&st.f_blocks.to_le_bytes());
            out[8..16].copy_from_slice(&st.f_bfree.to_le_bytes());
            out[16..24].copy_from_slice(&st.f_bavail.to_le_bytes());
            out[24..32].copy_from_slice(&st.f_files.to_le_bytes());
            out[32..40].copy_from_slice(&st.f_ffree.to_le_bytes());
            out[40..44].copy_from_slice(&st.f_bsize.to_le_bytes());
            out[44..48].copy_from_slice(&255u32.to_le_bytes()); // namelen
            out[48..52].copy_from_slice(&(st.f_iosize as u32).to_le_bytes());
            fuse::reply(hdr.unique, 0, &out)
        }
        Err(error) => fuse::reply(hdr.unique, sys::errno(&error), &[]),
    }
}
