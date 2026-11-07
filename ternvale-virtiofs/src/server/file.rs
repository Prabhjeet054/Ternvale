//! OPEN, RELEASE, READ, WRITE, FLUSH, FSYNC.

use crate::fuse::{self, InHeader};
use crate::handle::Handles;
use crate::opcode;
use crate::server::{dup_fd, Server};
use crate::sys;
use crate::wire::{u32_at, u64_at};

const MAX_IO: usize = 1024 * 1024;

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn open(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    let flags = u32_at(body, 0);
    if server.read_only && flags & (libc::O_WRONLY as u32 | libc::O_RDWR as u32) != 0 {
        tracing::warn!(
            target: "ternvale::virtiofs",
            reason = "OPEN for write on read-only share",
            "rejected share open"
        );
        return fuse::reply(hdr.unique, libc::EROFS, &[]);
    }
    let Some(inode) = server.inodes.get(hdr.nodeid) else {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    let dir = hdr.opcode == opcode::OPENDIR;
    let is_dir = inode.mode & u32::from(libc::S_IFMT) == u32::from(libc::S_IFDIR);
    if dir && !is_dir {
        tracing::warn!(
            target: "ternvale::virtiofs",
            reason = "OPENDIR on a non-directory",
            "rejected share open"
        );
        return fuse::reply(hdr.unique, libc::ENOTDIR, &[]);
    }
    if !dir && is_dir {
        tracing::warn!(
            target: "ternvale::virtiofs",
            reason = "OPEN on a directory",
            "rejected share open"
        );
        return fuse::reply(hdr.unique, libc::EISDIR, &[]);
    }
    let fd = match dup_fd(inode.raw()) {
        Ok(fd) => fd,
        Err(errno) => return fuse::reply(hdr.unique, errno, &[]),
    };
    match server.handles.insert(hdr.nodeid, fd, dir) {
        Ok(fh) => {
            let mut out = [0u8; 16];
            out[0..8].copy_from_slice(&fh.to_le_bytes());
            fuse::reply(hdr.unique, 0, &out)
        }
        Err(errno) => fuse::reply(hdr.unique, errno, &[]),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn release(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    let fh = u64_at(body, 0);
    match server.handles.release(fh) {
        Ok(()) => fuse::reply(hdr.unique, 0, &[]),
        Err(errno) => fuse::reply(hdr.unique, errno, &[]),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn read(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    let fh = u64_at(body, 0);
    let offset = u64_at(body, 8);
    let size = u32_at(body, 16) as usize;
    let size = size.min(MAX_IO);
    let Some(handle) = server.handles.get(fh) else {
        return fuse::reply(hdr.unique, libc::EBADF, &[]);
    };
    tracing::trace!(
        target: "ternvale::virtiofs",
        fh,
        nodeid = handle.nodeid,
        "share read"
    );
    if handle.dir {
        tracing::warn!(
            target: "ternvale::virtiofs",
            reason = "READ on a directory handle",
            "rejected share read"
        );
        return fuse::reply(hdr.unique, libc::EISDIR, &[]);
    }
    let mut buf = vec![0u8; size];
    match sys::pread(Handles::raw(handle), &mut buf, offset) {
        Ok(n) => {
            buf.truncate(n);
            fuse::reply(hdr.unique, 0, &buf)
        }
        Err(error) => fuse::reply(hdr.unique, sys::errno(&error), &[]),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn write(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    if let Err(reply) = server.want_write(hdr.unique) {
        return reply;
    }
    // fuse_write_in is 40 bytes on 7.9+.
    if body.len() < 24 {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    }
    let fh = u64_at(body, 0);
    let offset = u64_at(body, 8);
    let size = u32_at(body, 16) as usize;
    let header = if body.len() >= 40 { 40 } else { 24 };
    let data = if header <= body.len() {
        &body[header..]
    } else {
        &[]
    };
    let take = size.min(data.len()).min(MAX_IO);
    let Some(handle) = server.handles.get(fh) else {
        return fuse::reply(hdr.unique, libc::EBADF, &[]);
    };
    match sys::pwrite(Handles::raw(handle), &data[..take], offset) {
        Ok(n) => fuse::reply(hdr.unique, 0, &(n as u32).to_le_bytes()),
        Err(error) => fuse::reply(hdr.unique, sys::errno(&error), &[]),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn flush(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    fsync(server, hdr, body)
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn fsync(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    let fh = u64_at(body, 0);
    let Some(handle) = server.handles.get(fh) else {
        return fuse::reply(hdr.unique, libc::EBADF, &[]);
    };
    match sys::fsync(Handles::raw(handle)) {
        Ok(()) => fuse::reply(hdr.unique, 0, &[]),
        Err(error) => fuse::reply(hdr.unique, sys::errno(&error), &[]),
    }
}
