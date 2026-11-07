//! Open file-handle table. Caps concurrent opens so a guest cannot exhaust
//! host fds.

use std::collections::HashMap;
use std::os::unix::io::{OwnedFd, RawFd};

use crate::sys;

/// Default cap (also used when the share does not override it).
pub const DEFAULT_MAX: usize = 256;

/// One `OPEN`/`CREATE`/`OPENDIR` handle.
pub struct Handle {
    pub nodeid: u64,
    pub fd: OwnedFd,
    pub dir: bool,
}

/// Guest `fh` → handle.
pub struct Handles {
    next: u64,
    max: usize,
    by_fh: HashMap<u64, Handle>,
}

impl Handles {
    #[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(max))]
    pub fn new(max: usize) -> Self {
        Self {
            next: 1,
            max: max.max(1),
            by_fh: HashMap::new(),
        }
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtiofs",
        skip_all,
        fields(nodeid, dir)
    )]
    pub fn insert(&mut self, nodeid: u64, fd: OwnedFd, dir: bool) -> Result<u64, i32> {
        if self.by_fh.len() >= self.max {
            tracing::warn!(
                target: "ternvale::virtiofs",
                max = self.max,
                reason = "open handle cap reached",
                "rejected share open"
            );
            return Err(libc::ENFILE);
        }
        let fh = self.next;
        self.next = self.next.saturating_add(1);
        self.by_fh.insert(fh, Handle { nodeid, fd, dir });
        tracing::debug!(target: "ternvale::virtiofs", fh, nodeid, dir, "handle opened");
        Ok(fh)
    }

    #[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all, fields(fh))]
    pub fn get(&self, fh: u64) -> Option<&Handle> {
        self.by_fh.get(&fh)
    }

    #[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(fh))]
    pub fn release(&mut self, fh: u64) -> Result<(), i32> {
        if self.by_fh.remove(&fh).is_none() {
            tracing::warn!(
                target: "ternvale::virtiofs",
                fh,
                reason = "RELEASE of unknown fh",
                "rejected share release"
            );
            return Err(libc::EBADF);
        }
        Ok(())
    }

    #[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all)]
    pub fn raw(handle: &Handle) -> RawFd {
        sys::raw(&handle.fd)
    }
}

#[cfg(test)]
mod tests {
    use super::Handles;
    use crate::sys;

    #[test]
    fn cap_rejects_further_opens() {
        let dir = std::env::temp_dir().join(format!("tv-fs-fh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let mut handles = Handles::new(1);
        let a = sys::open_dir(&dir).expect("a");
        let b = sys::open_dir(&dir).expect("b");
        assert!(handles.insert(1, a, true).is_ok());
        assert_eq!(handles.insert(1, b, true).unwrap_err(), libc::ENFILE);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
