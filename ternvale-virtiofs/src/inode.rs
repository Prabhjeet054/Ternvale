//! Inode table: generation numbers and FUSE LOOKUP/FORGET reference counts.

use std::collections::HashMap;
use std::os::unix::io::{OwnedFd, RawFd};
use std::path::PathBuf;

use crate::fuse::{self, Attr};
use crate::sys;

/// FUSE node id 1 is the share root.
pub const ROOT_ID: u64 = fuse::ROOT_ID;

/// One exported inode. The fd is a directory, file, or symlink opened with
/// `O_NOFOLLOW` / `O_SYMLINK` from the parent, then checked against the root.
pub struct Inode {
    pub nodeid: u64,
    pub generation: u64,
    pub nlookup: u64,
    pub fd: OwnedFd,
    pub mode: u32,
}

/// Live inodes plus the next id/generation.
pub struct Table {
    root_real: PathBuf,
    next_id: u64,
    next_gen: u64,
    by_id: HashMap<u64, Inode>,
}

impl Table {
    /// Root directory fd already opened and verified.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtiofs",
        skip_all,
        fields(root = %root_real.display())
    )]
    pub fn new(root_fd: OwnedFd, root_real: PathBuf) -> Self {
        let st = sys::fstat(sys::raw(&root_fd)).ok();
        let mode = st.map(|s| u32::from(s.st_mode)).unwrap_or(0o40755);
        let mut by_id = HashMap::new();
        by_id.insert(
            ROOT_ID,
            Inode {
                nodeid: ROOT_ID,
                generation: 1,
                nlookup: 1,
                fd: root_fd,
                mode,
            },
        );
        Self {
            root_real,
            next_id: 2,
            next_gen: 2,
            by_id,
        }
    }

    #[tracing::instrument(
        level = "trace",
        target = "ternvale::virtiofs",
        skip_all,
        fields(nodeid)
    )]
    pub fn get(&self, nodeid: u64) -> Option<&Inode> {
        self.by_id.get(&nodeid)
    }

    /// Real host path of the share root.
    #[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all)]
    pub fn root(&self) -> &PathBuf {
        &self.root_real
    }

    /// Insert a newly opened child. Returns `(nodeid, generation)`.
    #[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
    pub fn insert(&mut self, fd: OwnedFd, mode: u32) -> Result<(u64, u64), i32> {
        let path = sys::fd_path(sys::raw(&fd)).map_err(|e| sys::errno(&e))?;
        if !sys::under_root(&path, &self.root_real) {
            tracing::warn!(
                target: "ternvale::virtiofs",
                path = %path.display(),
                root = %self.root_real.display(),
                reason = "fd path escaped the share root",
                "rejected share lookup"
            );
            return Err(libc::EPERM);
        }
        if self.by_id.len() >= 4096 {
            tracing::warn!(
                target: "ternvale::virtiofs",
                reason = "inode table full",
                "rejected share lookup"
            );
            return Err(libc::ENFILE);
        }
        let nodeid = self.next_id;
        self.next_id += 1;
        let generation = self.next_gen;
        self.next_gen += 1;
        self.by_id.insert(
            nodeid,
            Inode {
                nodeid,
                generation,
                nlookup: 1,
                fd,
                mode,
            },
        );
        tracing::debug!(
            target: "ternvale::virtiofs",
            nodeid,
            generation,
            mode,
            path = %path.display(),
            "inode inserted"
        );
        Ok((nodeid, generation))
    }

    /// `FUSE_FORGET`: drop when `nlookup` reaches 0. Root is never dropped.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtiofs",
        skip_all,
        fields(nodeid, nlookup)
    )]
    pub fn forget(&mut self, nodeid: u64, nlookup: u64) {
        if nodeid == ROOT_ID {
            return;
        }
        let Some(inode) = self.by_id.get_mut(&nodeid) else {
            tracing::warn!(
                target: "ternvale::virtiofs",
                nodeid,
                reason = "FORGET for unknown inode",
                "rejected share forget"
            );
            return;
        };
        inode.nlookup = inode.nlookup.saturating_sub(nlookup);
        if inode.nlookup == 0 {
            self.by_id.remove(&nodeid);
            tracing::debug!(target: "ternvale::virtiofs", nodeid, "inode forgotten");
        }
    }
}

impl Inode {
    #[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all)]
    pub fn raw(&self) -> RawFd {
        sys::raw(&self.fd)
    }
}

/// `fuse_attr` from `fstat`, with uid/gid replaced by the host user.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtiofs",
    skip_all,
    fields(nodeid)
)]
pub fn attr_of(inode: &Inode) -> Result<Attr, i32> {
    tracing::trace!(
        target: "ternvale::virtiofs",
        nodeid = inode.nodeid,
        generation = inode.generation,
        "fuse attr"
    );
    let st = sys::fstat(inode.raw()).map_err(|e| sys::errno(&e))?;
    let (uid, gid) = sys::host_ids();
    Ok(stat_to_attr(inode.nodeid, &st, uid, gid))
}

#[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all)]
pub fn stat_to_attr(ino: u64, st: &libc::stat, uid: u32, gid: u32) -> Attr {
    Attr {
        ino,
        size: st.st_size as u64,
        blocks: st.st_blocks as u64,
        atime: st.st_atime as u64,
        mtime: st.st_mtime as u64,
        ctime: st.st_ctime as u64,
        atimensec: st.st_atime_nsec as u32,
        mtimensec: st.st_mtime_nsec as u32,
        ctimensec: st.st_ctime_nsec as u32,
        mode: u32::from(st.st_mode),
        nlink: st.st_nlink as u32,
        uid,
        gid,
        rdev: st.st_rdev as u32,
        blksize: st.st_blksize as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::{Table, ROOT_ID};
    use crate::sys;

    #[test]
    fn forget_drops_child_not_root() {
        let dir = std::env::temp_dir().join(format!("tv-fs-ino-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let fd = sys::open_dir(&dir).expect("root");
        let real = sys::fd_path(sys::raw(&fd)).expect("path");
        let mut table = Table::new(fd, real);
        table.forget(ROOT_ID, 99);
        assert!(table.get(ROOT_ID).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
