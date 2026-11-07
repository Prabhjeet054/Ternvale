//! In-process FUSE server for one share.

use std::os::unix::io::OwnedFd;
use std::path::Path;

use crate::error::FsError;
use crate::fuse::{self, InHeader};
use crate::handle::Handles;
use crate::inode::Table;
use crate::opcode;
use crate::sys;

mod access;
mod dir;
mod file;
mod init;
mod link;
mod lookup;
mod meta;

/// Options for one export.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Guest writes, creates, and xattr updates fail with `EROFS`.
    pub read_only: bool,
    /// Cap on concurrent OPEN/CREATE/OPENDIR handles.
    pub max_handles: usize,
    /// Windows filename policy (reserved names, illegal chars, MAX_PATH,
    /// case-insensitive lookup/create).
    pub windows_names: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            read_only: false,
            max_handles: crate::handle::DEFAULT_MAX,
            windows_names: false,
        }
    }
}

/// One FUSE session.
pub struct Server {
    pub(crate) inodes: Table,
    pub(crate) handles: Handles,
    pub(crate) read_only: bool,
    pub(crate) windows_names: bool,
    pub(crate) inited: bool,
}

impl Server {
    /// Open `root` with `openat`-style directory fd and remember its real path.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtiofs",
        skip_all,
        fields(root = %root.display(), read_only = opts.read_only, windows_names = opts.windows_names)
    )]
    pub fn open(root: &Path, opts: Options) -> Result<Self, FsError> {
        let fd = sys::open_dir(root).map_err(|source| FsError::Export {
            path: root.to_path_buf(),
            source,
        })?;
        let real = sys::fd_path(sys::raw(&fd)).map_err(|source| FsError::Export {
            path: root.to_path_buf(),
            source,
        })?;
        tracing::info!(
            target: "ternvale::virtiofs",
            root = %real.display(),
            read_only = opts.read_only,
            max_handles = opts.max_handles,
            windows_names = opts.windows_names,
            "virtio-fs export ready"
        );
        Ok(Self {
            inodes: Table::new(fd, real),
            handles: Handles::new(opts.max_handles),
            read_only: opts.read_only,
            windows_names: opts.windows_names,
            inited: false,
        })
    }

    /// Handle one FUSE request. `None` means FORGET (no reply).
    #[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all, fields(len = request.len()))]
    pub fn handle(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        let Some(hdr) = InHeader::parse(request) else {
            tracing::warn!(
                target: "ternvale::virtiofs",
                reason = "short FUSE in-header",
                "rejected share request"
            );
            return Some(fuse::reply(0, libc::EINVAL, &[]));
        };
        tracing::trace!(
            target: "ternvale::virtiofs",
            opcode = opcode::name(hdr.opcode),
            unique = hdr.unique,
            nodeid = hdr.nodeid,
            len = hdr.len,
            "fuse request"
        );
        let body = if request.len() >= fuse::IN_HEADER {
            &request[fuse::IN_HEADER..]
        } else {
            &[]
        };
        if hdr.opcode == opcode::FORGET {
            lookup::forget(self, hdr, body);
            return None;
        }
        Some(self.dispatch(hdr, body))
    }

    fn dispatch(&mut self, hdr: InHeader, body: &[u8]) -> Vec<u8> {
        if hdr.opcode != opcode::INIT && !self.inited {
            tracing::warn!(
                target: "ternvale::virtiofs",
                opcode = opcode::name(hdr.opcode),
                reason = "FUSE request before INIT",
                "rejected share request"
            );
            return fuse::reply(hdr.unique, libc::EIO, &[]);
        }
        match hdr.opcode {
            opcode::INIT => init::init(self, hdr, body),
            opcode::LOOKUP => lookup::lookup(self, hdr, body),
            opcode::GETATTR => meta::getattr(self, hdr, body),
            opcode::SETATTR => meta::setattr(self, hdr, body),
            opcode::STATFS => meta::statfs(self, hdr),
            opcode::ACCESS => access::access(self, hdr, body),
            opcode::OPEN | opcode::OPENDIR => file::open(self, hdr, body),
            opcode::RELEASE | opcode::RELEASEDIR => file::release(self, hdr, body),
            opcode::READ => file::read(self, hdr, body),
            opcode::WRITE => file::write(self, hdr, body),
            opcode::FLUSH => file::flush(self, hdr, body),
            opcode::FSYNC | opcode::FSYNCDIR => file::fsync(self, hdr, body),
            opcode::MKDIR => dir::mkdir(self, hdr, body),
            opcode::CREATE => dir::create(self, hdr, body),
            opcode::UNLINK => dir::unlink(self, hdr, body, false),
            opcode::RMDIR => dir::unlink(self, hdr, body, true),
            opcode::RENAME => dir::rename(self, hdr, body, false),
            opcode::RENAME2 => dir::rename(self, hdr, body, true),
            opcode::READDIR => dir::readdir(self, hdr, body, false),
            opcode::READDIRPLUS => dir::readdir(self, hdr, body, true),
            opcode::SYMLINK => link::symlink(self, hdr, body),
            opcode::READLINK => link::readlink(self, hdr, body),
            opcode::GETXATTR => link::getxattr(self, hdr, body),
            opcode::SETXATTR => link::setxattr(self, hdr, body),
            opcode::LISTXATTR => link::listxattr(self, hdr, body),
            opcode::REMOVEXATTR => link::removexattr(self, hdr, body),
            other => {
                tracing::warn!(
                    target: "ternvale::virtiofs",
                    opcode = opcode::name(other),
                    reason = "unsupported FUSE opcode",
                    "rejected share request"
                );
                fuse::reply(hdr.unique, libc::ENOSYS, &[])
            }
        }
    }

    /// POSIX (+ Windows) name policy; returns the host directory entry to use.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtiofs",
        skip_all,
        fields(name, creating)
    )]
    pub(crate) fn child_name(
        &self,
        parent_id: u64,
        name: &str,
        creating: bool,
    ) -> Result<String, i32> {
        let Some(parent) = self.inodes.get(parent_id) else {
            return Err(libc::ENOENT);
        };
        if self.windows_names {
            if let Ok(path) = sys::fd_path(parent.raw()) {
                crate::name::check_windows_path(self.inodes.root(), &path, name)?;
            }
        }
        crate::name::prepare(parent.raw(), name, self.windows_names, creating)
    }

    pub(crate) fn want_write(&self, unique: u64) -> Result<(), Vec<u8>> {
        if self.read_only {
            tracing::warn!(
                target: "ternvale::virtiofs",
                reason = "share is read-only",
                "rejected share write"
            );
            Err(fuse::reply(unique, libc::EROFS, &[]))
        } else {
            Ok(())
        }
    }
}

/// Duplicate an inode fd for OPEN.
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub(crate) fn dup_fd(fd: i32) -> Result<OwnedFd, i32> {
    // SAFETY: `fd` is an inode-table descriptor; `dup` clones it.
    let n = unsafe { libc::dup(fd) };
    if n < 0 {
        Err(sys::errno(&std::io::Error::last_os_error()))
    } else {
        use std::os::unix::io::FromRawFd;
        // SAFETY: `dup` returned a new fd this function owns.
        Ok(unsafe { OwnedFd::from_raw_fd(n) })
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod windows_tests;
