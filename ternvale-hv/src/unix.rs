//! Unix-socket helpers used by the privileged helper (peer pid/uid, peek).

use std::io;
use std::os::fd::RawFd;

use super::ffi;

/// `LOCAL_PEERCRED` (`<sys/un.h>` on Darwin).
const LOCAL_PEERCRED: i32 = 0x001;
/// `LOCAL_PEERPID` (`<sys/un.h>` on Darwin).
const LOCAL_PEERPID: i32 = 0x002;
const SOL_LOCAL: i32 = 0;
/// `XUCRED_VERSION` (`<sys/ucred.h>` on Darwin).
const XUCRED_VERSION: u32 = 0;
/// `XU_NGROUPS`.
const XU_NGROUPS: usize = 16;

/// Darwin `struct xucred` (`<sys/ucred.h>`).
#[repr(C)]
struct Xucred {
    cr_version: u32,
    cr_uid: u32,
    cr_ngroups: i16,
    cr_groups: [u32; XU_NGROUPS],
}

unsafe extern "C" {
    fn getuid() -> u32;
}

/// PID of the process at the other end of a connected Unix socket.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all, fields(fd))]
pub fn unix_peer_pid(fd: RawFd) -> io::Result<u32> {
    let mut pid: u32 = 0;
    let mut len = std::mem::size_of::<u32>() as u32;
    // SAFETY: `fd` is a live socket; `pid`/`len` are valid out pointers for LOCAL_PEERPID.
    let rc = unsafe {
        ffi::getsockopt(
            fd,
            SOL_LOCAL,
            LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &raw mut len,
        )
    };
    tracing::trace!(
        target: "ternvale::hv",
        fd,
        rc,
        pid,
        "getsockopt LOCAL_PEERPID"
    );
    if rc != 0 {
        tracing::error!(target: "ternvale::hv", fd, rc, "LOCAL_PEERPID failed");
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "unix peer pid is 0",
        ));
    }
    Ok(pid)
}

/// Effective UID of this process (`getuid(2)`).
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all)]
pub fn host_uid() -> u32 {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    unsafe { getuid() }
}

/// Effective UID of the process at the other end of a connected Unix socket
/// (`LOCAL_PEERCRED` / `xucred.cr_uid`).
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all, fields(fd))]
pub fn unix_peer_uid(fd: RawFd) -> io::Result<u32> {
    let mut cred = Xucred {
        cr_version: 0,
        cr_uid: 0,
        cr_ngroups: 0,
        cr_groups: [0; XU_NGROUPS],
    };
    let mut len = std::mem::size_of::<Xucred>() as u32;
    // SAFETY: `fd` is a live socket; `cred`/`len` are valid out pointers for LOCAL_PEERCRED.
    let rc = unsafe {
        ffi::getsockopt(
            fd,
            SOL_LOCAL,
            LOCAL_PEERCRED,
            (&raw mut cred).cast(),
            &raw mut len,
        )
    };
    tracing::trace!(
        target: "ternvale::hv",
        fd,
        rc,
        uid = cred.cr_uid,
        version = cred.cr_version,
        "getsockopt LOCAL_PEERCRED"
    );
    if rc != 0 {
        tracing::error!(target: "ternvale::hv", fd, rc, "LOCAL_PEERCRED failed");
        return Err(io::Error::last_os_error());
    }
    if cred.cr_version != XUCRED_VERSION {
        tracing::warn!(
            target: "ternvale::hv",
            fd,
            version = cred.cr_version,
            "LOCAL_PEERCRED xucred version is not XUCRED_VERSION; using cr_uid anyway"
        );
    }
    Ok(cred.cr_uid)
}

/// True when [`unix_peer_uid`] equals [`host_uid`].
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all, fields(fd))]
pub fn unix_peer_is_same_user(fd: RawFd) -> io::Result<bool> {
    let peer = unix_peer_uid(fd)?;
    let host = host_uid();
    Ok(peer == host)
}

/// `recv(..., MSG_PEEK)` so the helper can dispatch on the first four bytes.
#[tracing::instrument(level = "trace", target = "ternvale::hv", skip_all, fields(fd, len = buf.len()))]
pub fn unix_peek(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: `fd` is a live socket; `buf` is writable for `buf.len()` bytes.
    let n = unsafe { ffi::recv(fd, buf.as_mut_ptr().cast(), buf.len(), ffi::MSG_PEEK) };
    tracing::trace!(target: "ternvale::hv", fd, n, "recv MSG_PEEK");
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(n as usize)
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    use super::{host_uid, unix_peek, unix_peer_is_same_user, unix_peer_pid, unix_peer_uid};

    #[test]
    fn peer_pid_is_this_process() {
        let (a, b) = UnixStream::pair().expect("pair");
        let pid = unix_peer_pid(a.as_raw_fd()).expect("peer");
        assert_eq!(pid, std::process::id());
        drop(b);
    }

    #[test]
    fn peer_uid_is_this_user() {
        let (a, b) = UnixStream::pair().expect("pair");
        let uid = unix_peer_uid(a.as_raw_fd()).expect("peer uid");
        assert_eq!(uid, host_uid());
        assert!(unix_peer_is_same_user(a.as_raw_fd()).expect("same"));
        drop(b);
    }

    #[test]
    fn peek_does_not_consume() {
        let (mut a, mut b) = UnixStream::pair().expect("pair");
        use std::io::Write;
        b.write_all(b"TVNO").expect("write");
        let mut buf = [0u8; 4];
        let n = unix_peek(a.as_raw_fd(), &mut buf).expect("peek");
        assert_eq!(&buf[..n], b"TVNO");
        let mut again = [0u8; 4];
        std::io::Read::read_exact(&mut a, &mut again).expect("read");
        assert_eq!(&again, b"TVNO");
    }
}
