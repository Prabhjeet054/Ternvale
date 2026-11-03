//! `preadv` / `pwritev` against guest GPA buffers.

use std::fs::File;
use std::io::{self, ErrorKind};
use std::os::fd::AsRawFd;

use libc::{iovec, preadv, pwritev};
use ternvale_snapshot::ImageStack;
use ternvale_vmm::GuestMemory;

use super::Buffer;

const IOV_MAX: usize = 64;

pub(super) fn vectored(
    file: &File,
    mem: &GuestMemory,
    bufs: &[Buffer],
    mut offset: u64,
    read: bool,
) -> io::Result<()> {
    let mut index = 0;
    while index < bufs.len() {
        let chunk = &bufs[index..bufs.len().min(index + IOV_MAX)];
        let mut iov = Vec::with_capacity(chunk.len());
        for buf in chunk {
            if buf.len == 0 {
                continue;
            }
            let ptr = mem
                .host_ptr(buf.addr, buf.len as usize)
                .map_err(|error| io::Error::new(ErrorKind::InvalidInput, error.to_string()))?;
            iov.push(iovec {
                iov_base: ptr.cast(),
                iov_len: buf.len as usize,
            });
        }
        if iov.is_empty() {
            index += chunk.len();
            continue;
        }
        let want: usize = iov.iter().map(|v| v.iov_len).sum();
        let mut done = 0usize;
        while done < want {
            let n = syscall(file, &iov, offset, read)?;
            if n == 0 {
                if read {
                    zero_rest(mem, chunk, done)?;
                    return Ok(());
                }
                return Err(io::Error::new(ErrorKind::WriteZero, "short pwritev"));
            }
            done += n;
            offset += n as u64;
            advance_iov(&mut iov, n);
        }
        index += chunk.len();
    }
    Ok(())
}

fn syscall(file: &File, iov: &[iovec], offset: u64, read: bool) -> io::Result<usize> {
    let fd = file.as_raw_fd();
    let count = iov.len() as i32;
    // SAFETY: `iov` bases are guest RAM from `host_ptr`; the region lives
    // until `GuestMemory` drops. `offset` is a host file position.
    let n = unsafe {
        if read {
            preadv(fd, iov.as_ptr(), count, offset as i64)
        } else {
            pwritev(fd, iov.as_ptr(), count, offset as i64)
        }
    };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

fn advance_iov(iov: &mut [iovec], mut skip: usize) {
    for vec in iov {
        if skip == 0 {
            break;
        }
        if skip >= vec.iov_len {
            skip -= vec.iov_len;
            vec.iov_len = 0;
            vec.iov_base = std::ptr::null_mut();
        } else {
            vec.iov_base = (vec.iov_base as usize + skip) as *mut libc::c_void;
            vec.iov_len -= skip;
            skip = 0;
        }
    }
}

fn zero_rest(mem: &GuestMemory, bufs: &[Buffer], mut skip: usize) -> io::Result<()> {
    for buf in bufs {
        let len = buf.len as usize;
        if skip >= len {
            skip -= len;
            continue;
        }
        let ptr = mem
            .host_ptr(buf.addr + skip as u64, len - skip)
            .map_err(|error| io::Error::new(ErrorKind::InvalidInput, error.to_string()))?;
        // SAFETY: `host_ptr` validated the GPA range.
        unsafe {
            std::ptr::write_bytes(ptr, 0, len - skip);
        }
        skip = 0;
    }
    Ok(())
}

pub(super) fn copy_read_guest(
    stack: &ImageStack,
    mem: &GuestMemory,
    bufs: &[Buffer],
    mut offset: u64,
) -> io::Result<()> {
    for buf in bufs {
        if buf.len == 0 {
            continue;
        }
        let mut tmp = vec![0u8; buf.len as usize];
        stack.read_at(&mut tmp, offset)?;
        let ptr = mem
            .host_ptr(buf.addr, buf.len as usize)
            .map_err(|error| io::Error::new(ErrorKind::InvalidInput, error.to_string()))?;
        // SAFETY: `host_ptr` validated the range; copy into guest RAM.
        unsafe {
            std::ptr::copy_nonoverlapping(tmp.as_ptr(), ptr, tmp.len());
        }
        offset += u64::from(buf.len);
    }
    Ok(())
}

pub(super) fn copy_write_guest(
    stack: &mut ImageStack,
    mem: &GuestMemory,
    bufs: &[Buffer],
    mut offset: u64,
) -> io::Result<()> {
    for buf in bufs {
        if buf.len == 0 {
            continue;
        }
        let mut tmp = vec![0u8; buf.len as usize];
        mem.read_bytes(buf.addr, &mut tmp)
            .map_err(|error| io::Error::new(ErrorKind::InvalidInput, error.to_string()))?;
        stack.write_at(&tmp, offset)?;
        offset += u64::from(buf.len);
    }
    Ok(())
}
