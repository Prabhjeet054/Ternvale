//! Unix-socket helpers used by the privileged helper (peer pid, peek).

use std::io;
use std::os::fd::RawFd;

use super::ffi;

/// `LOCAL_PEERPID` (`<sys/un.h>` on Darwin).
const LOCAL_PEERPID: i32 = 0x002;
const SOL_LOCAL: i32 = 0;

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

    use super::{unix_peek, unix_peer_pid};

    #[test]
    fn peer_pid_is_this_process() {
        let (a, b) = UnixStream::pair().expect("pair");
        let pid = unix_peer_pid(a.as_raw_fd()).expect("peer");
        assert_eq!(pid, std::process::id());
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
