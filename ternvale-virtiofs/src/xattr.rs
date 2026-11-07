//! Darwin `fgetxattr` / `fsetxattr` / `flistxattr` / `fremovexattr`.

use std::ffi::CString;
use std::io;
use std::os::unix::io::RawFd;

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(name, size = buf.len()))]
pub fn get(fd: RawFd, name: &str, buf: &mut [u8]) -> Result<usize, io::Error> {
    let c = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `buf` is writable; `name` is a C string. position/options are 0.
    let n = unsafe { libc::fgetxattr(fd, c.as_ptr(), buf.as_mut_ptr().cast(), buf.len(), 0, 0) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(name, size = buf.len()))]
pub fn set(fd: RawFd, name: &str, buf: &[u8]) -> Result<(), io::Error> {
    let c = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `buf` is readable.
    let rc = unsafe { libc::fsetxattr(fd, c.as_ptr(), buf.as_ptr().cast(), buf.len(), 0, 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub fn list(fd: RawFd, buf: &mut [u8]) -> Result<usize, io::Error> {
    // SAFETY: `buf` is writable.
    let n = unsafe { libc::flistxattr(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(name))]
pub fn remove(fd: RawFd, name: &str) -> Result<(), io::Error> {
    let c = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `name` is a C string; `fd` is open.
    let rc = unsafe { libc::fremovexattr(fd, c.as_ptr(), 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
