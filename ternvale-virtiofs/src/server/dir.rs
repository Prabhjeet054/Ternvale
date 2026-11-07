//! MKDIR, CREATE, UNLINK, RMDIR, RENAME, READDIR(PLUS).

use crate::fuse::{self, c_string, InHeader};
use crate::inode;
use crate::server::Server;
use crate::sys;
use crate::wire::{u32_at, u64_at};

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn mkdir(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    if let Err(reply) = server.want_write(hdr.unique) {
        return reply;
    }
    if body.len() < 8 {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    }
    let mode = u32_at(body, 0);
    let Some(raw) = c_string(&body[8..]) else {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    };
    let name = match server.child_name(hdr.nodeid, raw, true) {
        Ok(name) => name,
        Err(errno) => return fuse::reply(hdr.unique, errno, &[]),
    };
    let Some(parent) = server.inodes.get(hdr.nodeid) else {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    if let Err(error) = sys::mkdirat(parent.raw(), &name, mode & 0o7777) {
        tracing::warn!(
            target: "ternvale::virtiofs",
            name = %name,
            error = %error,
            reason = "mkdirat failed",
            "rejected share mkdir"
        );
        return fuse::reply(hdr.unique, sys::errno(&error), &[]);
    }
    lookup_child(server, hdr, &name)
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn create(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    if let Err(reply) = server.want_write(hdr.unique) {
        return reply;
    }
    // flags, mode, umask, padding (16) then name.
    if body.len() < 16 {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    }
    let mode = u32_at(body, 4);
    let Some(raw) = c_string(&body[16..]) else {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    };
    let name = match server.child_name(hdr.nodeid, raw, true) {
        Ok(name) => name,
        Err(errno) => return fuse::reply(hdr.unique, errno, &[]),
    };
    let Some(parent) = server.inodes.get(hdr.nodeid) else {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    let flags = libc::O_EXCL | libc::O_RDWR;
    let child = match sys::openat_create(parent.raw(), &name, flags, mode & 0o7777) {
        Ok(fd) => fd,
        Err(error) => {
            tracing::warn!(
                target: "ternvale::virtiofs",
                name = %name,
                error = %error,
                reason = "create openat failed",
                "rejected share create"
            );
            return fuse::reply(hdr.unique, sys::errno(&error), &[]);
        }
    };
    let st = match sys::fstat(sys::raw(&child)) {
        Ok(st) => st,
        Err(error) => return fuse::reply(hdr.unique, sys::errno(&error), &[]),
    };
    let (nodeid, generation) = match server.inodes.insert(child, u32::from(st.st_mode)) {
        Ok(ids) => ids,
        Err(errno) => return fuse::reply(hdr.unique, errno, &[]),
    };
    let inode_fd = match server.inodes.get(nodeid) {
        Some(inode) => inode.raw(),
        None => return fuse::reply(hdr.unique, libc::ENOENT, &[]),
    };
    let open_fd = match crate::server::dup_fd(inode_fd) {
        Ok(fd) => fd,
        Err(errno) => return fuse::reply(hdr.unique, errno, &[]),
    };
    let fh = match server.handles.insert(nodeid, open_fd, false) {
        Ok(fh) => fh,
        Err(errno) => return fuse::reply(hdr.unique, errno, &[]),
    };
    let attr = match server
        .inodes
        .get(nodeid)
        .and_then(|i| inode::attr_of(i).ok())
    {
        Some(attr) => attr,
        None => return fuse::reply(hdr.unique, libc::ENOENT, &[]),
    };
    let mut payload = fuse::encode_entry(nodeid, generation, &attr);
    payload.extend_from_slice(&fh.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
    fuse::reply(hdr.unique, 0, &payload)
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn unlink(server: &mut Server, hdr: InHeader, body: &[u8], rmdir: bool) -> Vec<u8> {
    if let Err(reply) = server.want_write(hdr.unique) {
        return reply;
    }
    let Some(raw) = c_string(body) else {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    };
    let name = match server.child_name(hdr.nodeid, raw, false) {
        Ok(name) => name,
        Err(errno) => return fuse::reply(hdr.unique, errno, &[]),
    };
    let Some(parent) = server.inodes.get(hdr.nodeid) else {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    match sys::unlinkat(parent.raw(), &name, rmdir) {
        Ok(()) => fuse::reply(hdr.unique, 0, &[]),
        Err(error) => {
            tracing::warn!(
                target: "ternvale::virtiofs",
                name = %name,
                error = %error,
                reason = "unlinkat failed",
                "rejected share unlink"
            );
            fuse::reply(hdr.unique, sys::errno(&error), &[])
        }
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn rename(server: &mut Server, hdr: InHeader, body: &[u8], rename2: bool) -> Vec<u8> {
    if let Err(reply) = server.want_write(hdr.unique) {
        return reply;
    }
    // fuse_rename_in: newdir (u64) then oldname\0 newname\0
    // fuse_rename2_in: newdir, flags, padding, names
    let names_off = if rename2 { 16 } else { 8 };
    if body.len() < names_off {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    }
    let newdir = u64_at(body, 0);
    let names = &body[names_off..];
    let Some(old_end) = names.iter().position(|b| *b == 0) else {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    };
    let Some(old) = std::str::from_utf8(&names[..old_end]).ok() else {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    };
    let rest = if old_end + 1 < names.len() {
        &names[old_end + 1..]
    } else {
        &[]
    };
    let Some(new) = c_string(rest) else {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    };
    let old_name = match server.child_name(hdr.nodeid, old, false) {
        Ok(name) => name,
        Err(errno) => return fuse::reply(hdr.unique, errno, &[]),
    };
    let new_name = match server.child_name(newdir, new, false) {
        Ok(name) => name,
        Err(errno) => return fuse::reply(hdr.unique, errno, &[]),
    };
    let Some(src) = server.inodes.get(hdr.nodeid) else {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    let Some(dst) = server.inodes.get(newdir) else {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    let src_fd = src.raw();
    let dst_fd = dst.raw();
    match sys::renameat(src_fd, &old_name, dst_fd, &new_name) {
        Ok(()) => fuse::reply(hdr.unique, 0, &[]),
        Err(error) => {
            tracing::warn!(
                target: "ternvale::virtiofs",
                old = %old_name,
                new = %new_name,
                error = %error,
                reason = "renameat failed",
                "rejected share rename"
            );
            fuse::reply(hdr.unique, sys::errno(&error), &[])
        }
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn readdir(server: &mut Server, hdr: InHeader, body: &[u8], plus: bool) -> Vec<u8> {
    let fh = u64_at(body, 0);
    let offset = u64_at(body, 8);
    let size = u32_at(body, 16) as usize;
    let Some(handle) = server.handles.get(fh) else {
        return fuse::reply(hdr.unique, libc::EBADF, &[]);
    };
    let dir_fd = crate::handle::Handles::raw(handle);
    let names = match sys::readdir_names(dir_fd) {
        Ok(n) => n,
        Err(error) => return fuse::reply(hdr.unique, sys::errno(&error), &[]),
    };
    let mut out = Vec::new();
    for (i, (name, d_type)) in names.iter().enumerate() {
        let off = (i as u64) + 1;
        if off <= offset {
            continue;
        }
        let typ = u32::from(*d_type);
        let rec = if plus {
            let mut rec = fuse::encode_entry(0, 0, &placeholder_attr());
            rec.extend_from_slice(&fuse::dirent_record(0, off, typ, name));
            rec
        } else {
            fuse::dirent_record(0, off, typ, name)
        };
        if out.len() + rec.len() > size {
            break;
        }
        out.extend_from_slice(&rec);
    }
    fuse::reply(hdr.unique, 0, &out)
}

fn lookup_child(server: &mut Server, hdr: InHeader, name: &str) -> Vec<u8> {
    let mut body = name.as_bytes().to_vec();
    body.push(0);
    crate::server::lookup::lookup(server, hdr, &body)
}

fn placeholder_attr() -> fuse::Attr {
    fuse::Attr {
        ino: 0,
        size: 0,
        blocks: 0,
        atime: 0,
        mtime: 0,
        ctime: 0,
        atimensec: 0,
        mtimensec: 0,
        ctimensec: 0,
        mode: 0,
        nlink: 1,
        uid: 0,
        gid: 0,
        rdev: 0,
        blksize: 4096,
    }
}
