//! SYMLINK, READLINK, and extended attributes.

use crate::fuse::{self, c_string, InHeader};
use crate::name;
use crate::server::Server;
use crate::sys;
use crate::wire::u32_at;
use crate::xattr;

const MAX_XATTR: usize = 64 * 1024;

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn symlink(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    if let Err(reply) = server.want_write(hdr.unique) {
        return reply;
    }
    let Some(name_end) = body.iter().position(|b| *b == 0) else {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    };
    let Some(link_name) = std::str::from_utf8(&body[..name_end]).ok() else {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    };
    let rest = if name_end + 1 < body.len() {
        &body[name_end + 1..]
    } else {
        &[]
    };
    let Some(target) = c_string(rest) else {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    };
    if let Err(errno) = name::check(link_name) {
        return fuse::reply(hdr.unique, errno, &[]);
    }
    if target.is_empty() || target.len() > 1024 {
        tracing::warn!(
            target: "ternvale::virtiofs",
            reason = "symlink target empty or too long",
            "rejected share symlink"
        );
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    }
    let Some(parent) = server.inodes.get(hdr.nodeid) else {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    if let Err(error) = sys::symlinkat(parent.raw(), link_name, target) {
        tracing::warn!(
            target: "ternvale::virtiofs",
            name = link_name,
            error = %error,
            reason = "symlinkat failed",
            "rejected share symlink"
        );
        return fuse::reply(hdr.unique, sys::errno(&error), &[]);
    }
    let mut lookup_body = link_name.as_bytes().to_vec();
    lookup_body.push(0);
    crate::server::lookup::lookup(server, hdr, &lookup_body)
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn readlink(server: &mut Server, hdr: InHeader, _body: &[u8]) -> Vec<u8> {
    let Some(inode) = server.inodes.get(hdr.nodeid) else {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    match sys::readlink_fd(inode.raw()) {
        Ok(target) => fuse::reply(hdr.unique, 0, &target),
        Err(error) => {
            tracing::warn!(
                target: "ternvale::virtiofs",
                error = %error,
                reason = "readlink failed",
                "rejected share readlink"
            );
            fuse::reply(hdr.unique, sys::errno(&error), &[])
        }
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn getxattr(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    if body.len() < 8 {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    }
    let size = u32_at(body, 0) as usize;
    let Some(xname) = c_string(&body[8..]) else {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    };
    let Some(inode) = server.inodes.get(hdr.nodeid) else {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    if size == 0 {
        return match xattr::get(inode.raw(), xname, &mut []) {
            Ok(n) => {
                let mut out = [0u8; 8];
                out[0..4].copy_from_slice(&(n as u32).to_le_bytes());
                fuse::reply(hdr.unique, 0, &out)
            }
            Err(error) => fuse::reply(hdr.unique, sys::errno(&error), &[]),
        };
    }
    let mut buf = vec![0u8; size.min(MAX_XATTR)];
    match xattr::get(inode.raw(), xname, &mut buf) {
        Ok(n) => {
            buf.truncate(n);
            fuse::reply(hdr.unique, 0, &buf)
        }
        Err(error) => fuse::reply(hdr.unique, sys::errno(&error), &[]),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn setxattr(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    if let Err(reply) = server.want_write(hdr.unique) {
        return reply;
    }
    if body.len() < 8 {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    }
    let size = u32_at(body, 0) as usize;
    let Some(xname) = c_string(&body[8..]) else {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    };
    let name_end = body[8..].iter().position(|b| *b == 0).unwrap_or(0);
    let value_off = 8 + name_end + 1;
    let value = if value_off < body.len() {
        &body[value_off..]
    } else {
        &[]
    };
    let take = size.min(value.len()).min(MAX_XATTR);
    let Some(inode) = server.inodes.get(hdr.nodeid) else {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    match xattr::set(inode.raw(), xname, &value[..take]) {
        Ok(()) => fuse::reply(hdr.unique, 0, &[]),
        Err(error) => {
            tracing::warn!(
                target: "ternvale::virtiofs",
                error = %error,
                reason = "setxattr failed",
                "rejected share setxattr"
            );
            fuse::reply(hdr.unique, sys::errno(&error), &[])
        }
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn listxattr(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    let size = u32_at(body, 0) as usize;
    let Some(inode) = server.inodes.get(hdr.nodeid) else {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    if size == 0 {
        return match xattr::list(inode.raw(), &mut []) {
            Ok(n) => {
                let mut out = [0u8; 8];
                out[0..4].copy_from_slice(&(n as u32).to_le_bytes());
                fuse::reply(hdr.unique, 0, &out)
            }
            Err(error) => fuse::reply(hdr.unique, sys::errno(&error), &[]),
        };
    }
    let mut buf = vec![0u8; size.min(MAX_XATTR)];
    match xattr::list(inode.raw(), &mut buf) {
        Ok(n) => {
            buf.truncate(n);
            fuse::reply(hdr.unique, 0, &buf)
        }
        Err(error) => fuse::reply(hdr.unique, sys::errno(&error), &[]),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(super) fn removexattr(server: &mut Server, hdr: InHeader, body: &[u8]) -> Vec<u8> {
    if let Err(reply) = server.want_write(hdr.unique) {
        return reply;
    }
    let Some(xname) = c_string(body) else {
        return fuse::reply(hdr.unique, libc::EINVAL, &[]);
    };
    let Some(inode) = server.inodes.get(hdr.nodeid) else {
        return fuse::reply(hdr.unique, libc::ENOENT, &[]);
    };
    match xattr::remove(inode.raw(), xname) {
        Ok(()) => fuse::reply(hdr.unique, 0, &[]),
        Err(error) => fuse::reply(hdr.unique, sys::errno(&error), &[]),
    }
}
