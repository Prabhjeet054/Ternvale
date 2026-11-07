//! Safe-ish wrappers around Darwin `openat` and friends. Each `unsafe` block
//! is a single libc call with a stated invariant. Hypervisor FFI stays in
//! `ternvale-hv`.

use std::ffi::CString;
use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

use crate::error::io_errno;

/// `F_GETPATH` (`<sys/fcntl.h>` on Darwin).
const F_GETPATH: i32 = 50;
/// `O_SYMLINK`: open the symlink inode itself.
const O_SYMLINK: i32 = 0x0020_0000;

/// Host uid/gid used in every FUSE `fuse_attr` (guest ids are not trusted).
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub fn host_ids() -> (u32, u32) {
    // SAFETY: getuid/getgid are pure reads of the process credentials.
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    (uid, gid)
}

/// Open `path` as a directory fd (`O_RDONLY | O_DIRECTORY | O_CLOEXEC`).
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(path = %path.display()))]
pub fn open_dir(path: &Path) -> Result<OwnedFd, io::Error> {
    let c = c_path(path)?;
    // SAFETY: `c` is a valid C string; flags only open a directory.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    owned(fd)
}

/// `openat(dir, name, flags | O_CLOEXEC | O_NOFOLLOW)`. On `ELOOP`, retry with
/// `O_SYMLINK` so LOOKUP can see a symlink without following it.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtiofs",
    skip_all,
    fields(name, flags)
)]
pub fn openat_nofollow(dir: RawFd, name: &str, flags: i32) -> Result<OwnedFd, io::Error> {
    openat_at(dir, name, flags, 0o666)
}

/// `openat` with `O_CREAT` and an explicit mode (CREATE).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtiofs",
    skip_all,
    fields(name, flags, mode)
)]
pub fn openat_create(dir: RawFd, name: &str, flags: i32, mode: u32) -> Result<OwnedFd, io::Error> {
    openat_at(dir, name, flags | libc::O_CREAT, mode)
}

fn openat_at(dir: RawFd, name: &str, flags: i32, mode: u32) -> Result<OwnedFd, io::Error> {
    let c = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let flags = flags | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    // SAFETY: `dir` is an open directory fd owned by the inode table; `c` lives
    // for the call. `O_NOFOLLOW` refuses to traverse a final symlink.
    let fd = unsafe { libc::openat(dir, c.as_ptr(), flags, mode as libc::c_uint) };
    if fd >= 0 {
        return owned(fd);
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ELOOP) {
        // Drop `O_NOFOLLOW` so Darwin `O_SYMLINK` can open the symlink inode.
        let link_flags = (flags & !libc::O_NOFOLLOW) | O_SYMLINK | libc::O_CLOEXEC;
        // SAFETY: `O_SYMLINK` opens the symlink itself and does not follow it.
        let fd = unsafe { libc::openat(dir, c.as_ptr(), link_flags, mode as libc::c_uint) };
        return owned(fd);
    }
    Err(err)
}

/// `fstat`.
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub fn fstat(fd: RawFd) -> Result<libc::stat, io::Error> {
    let mut st = unsafe { std::mem::zeroed() };
    // SAFETY: `st` is a valid `stat` out-parameter.
    let rc = unsafe { libc::fstat(fd, &mut st) };
    if rc == 0 {
        Ok(st)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Darwin `fcntl(F_GETPATH)`.
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub fn fd_path(fd: RawFd) -> Result<PathBuf, io::Error> {
    let mut buf = [0u8; 1024];
    // SAFETY: `buf` is 1024 bytes, the size `F_GETPATH` writes (MAXPATHLEN).
    let rc = unsafe { libc::fcntl(fd, F_GETPATH, buf.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let text =
        std::str::from_utf8(&buf[..end]).map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
    Ok(PathBuf::from(text))
}

/// True when `path` is `root` or a descendant (no `..` after canonicalize).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtiofs",
    skip_all,
    fields(path = %path.display(), root = %root.display())
)]
pub fn under_root(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

/// `mkdirat`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtiofs",
    skip_all,
    fields(name, mode)
)]
pub fn mkdirat(dir: RawFd, name: &str, mode: u32) -> Result<(), io::Error> {
    let c = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `dir` is an open directory; `c` is a single path component.
    let rc = unsafe { libc::mkdirat(dir, c.as_ptr(), mode as libc::mode_t) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `unlinkat`; `removedir` sets `AT_REMOVEDIR`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtiofs",
    skip_all,
    fields(name, removedir)
)]
pub fn unlinkat(dir: RawFd, name: &str, removedir: bool) -> Result<(), io::Error> {
    let c = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let flag = if removedir { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: `dir` is an open directory; name is a single component.
    let rc = unsafe { libc::unlinkat(dir, c.as_ptr(), flag) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `renameat`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtiofs",
    skip_all,
    fields(old, new)
)]
pub fn renameat(olddir: RawFd, old: &str, newdir: RawFd, new: &str) -> Result<(), io::Error> {
    let oc = CString::new(old).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let nc = CString::new(new).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: both dirs are open; names are single components.
    let rc = unsafe { libc::renameat(olddir, oc.as_ptr(), newdir, nc.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `symlinkat(target, dir, name)`.
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(name))]
pub fn symlinkat(dir: RawFd, name: &str, target: &str) -> Result<(), io::Error> {
    let t = CString::new(target).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let n = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `dir` is an open directory; strings are NUL-terminated.
    let rc = unsafe { libc::symlinkat(t.as_ptr(), dir, n.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `pread`.
#[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all, fields(len = buf.len(), off))]
pub fn pread(fd: RawFd, buf: &mut [u8], off: u64) -> Result<usize, io::Error> {
    // SAFETY: `buf` is writable for `buf.len()` bytes.
    let n = unsafe { libc::pread(fd, buf.as_mut_ptr().cast(), buf.len(), off as i64) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// `pwrite`.
#[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all, fields(len = buf.len(), off))]
pub fn pwrite(fd: RawFd, buf: &[u8], off: u64) -> Result<usize, io::Error> {
    // SAFETY: `buf` is readable for `buf.len()` bytes.
    let n = unsafe { libc::pwrite(fd, buf.as_ptr().cast(), buf.len(), off as i64) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// `fsync`.
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub fn fsync(fd: RawFd) -> Result<(), io::Error> {
    // SAFETY: `fd` is an open file the handle table owns.
    let rc = unsafe { libc::fsync(fd) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `ftruncate`.
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(size))]
pub fn ftruncate(fd: RawFd, size: u64) -> Result<(), io::Error> {
    // SAFETY: `fd` is writable; size is a guest-requested length already capped.
    let rc = unsafe { libc::ftruncate(fd, size as i64) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `fchmod`.
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(mode))]
pub fn fchmod(fd: RawFd, mode: u32) -> Result<(), io::Error> {
    // SAFETY: `fd` is open.
    let rc = unsafe { libc::fchmod(fd, mode as libc::mode_t) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `fstatfs`.
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub fn fstatfs(fd: RawFd) -> Result<libc::statfs, io::Error> {
    let mut st = unsafe { std::mem::zeroed() };
    // SAFETY: `st` is a valid `statfs` out-parameter.
    let rc = unsafe { libc::fstatfs(fd, &mut st) };
    if rc == 0 {
        Ok(st)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Directory listing via `fdopendir` on a duplicated fd.
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub fn readdir_names(dir: RawFd) -> Result<Vec<(String, u8)>, io::Error> {
    // SAFETY: `dup` clones the inode table's directory fd.
    let dup = unsafe { libc::dup(dir) };
    if dup < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fdopendir` takes ownership of `dup`.
    let dirp = unsafe { libc::fdopendir(dup) };
    if dirp.is_null() {
        let err = io::Error::last_os_error();
        // SAFETY: fdopendir failed, so we still own `dup`.
        unsafe { libc::close(dup) };
        return Err(err);
    }
    let mut names = Vec::new();
    loop {
        errno_clear();
        // SAFETY: `dirp` came from `fdopendir` and is not closed yet.
        let ent = unsafe { libc::readdir(dirp) };
        if ent.is_null() {
            break;
        }
        // SAFETY: `ent` is a live `dirent` until the next `readdir`/`closedir`.
        let d_type = unsafe { (*ent).d_type };
        let raw = unsafe { &(*ent).d_name };
        let bytes: Vec<u8> = raw
            .iter()
            .map(|c| *c as u8)
            .take_while(|b| *b != 0)
            .collect();
        let Ok(name) = String::from_utf8(bytes) else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        names.push((name, d_type));
    }
    // SAFETY: `dirp` is the only remaining owner of the duplicated fd.
    unsafe { libc::closedir(dirp) };
    Ok(names)
}

fn errno_clear() {
    // SAFETY: writing `errno` on this thread only.
    unsafe { *libc::__error() = 0 };
}

fn c_path(path: &Path) -> Result<CString, io::Error> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn owned(fd: RawFd) -> Result<OwnedFd, io::Error> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: `fd` is a freshly opened descriptor this function owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

/// `io_errno` helper for ops.
#[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all)]
pub fn errno(error: &io::Error) -> i32 {
    io_errno(error)
}

/// Raw fd of an [`OwnedFd`].
#[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all)]
pub fn raw(fd: &OwnedFd) -> RawFd {
    fd.as_raw_fd()
}

/// `readlink` of the path `F_GETPATH` reports for `fd` (symlink inode).
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all)]
pub fn readlink_fd(fd: RawFd) -> Result<Vec<u8>, io::Error> {
    let path = fd_path(fd)?;
    let c = c_path(&path)?;
    let mut buf = vec![0u8; 1024];
    // SAFETY: `buf` is writable; `c` is the symlink's own path, not a follow.
    let n = unsafe { libc::readlink(c.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        buf.truncate(n as usize);
        Ok(buf)
    }
}
