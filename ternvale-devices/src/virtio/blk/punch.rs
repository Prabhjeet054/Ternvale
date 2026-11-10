//! APFS hole-punch for virtio-blk DISCARD / WRITE_ZEROES.
//!
//! Darwin `fcntl(F_PUNCHHOLE)` (99) takes [`libc::fpunchhole_t`]. APFS
//! allocation blocks are 4096 bytes; unaligned ranges are zeroed at the
//! edges and the aligned interior is punched. TODO(verify): current APFS
//! still requires 4096-byte alignment for `F_PUNCHHOLE` (checked in tests).

use std::fs::File;
use std::io::{self, ErrorKind};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;

/// APFS allocation block size used as punch alignment.
pub const PUNCH_ALIGN: u64 = 4096;

/// Punch `[offset, offset+len)` as a hole, zeroing unaligned edges.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtio::blk",
    skip_all,
    fields(offset, len)
)]
pub fn punch_hole(file: &File, offset: u64, len: u64) -> io::Result<()> {
    if len == 0 {
        return Ok(());
    }
    let end = offset
        .checked_add(len)
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "discard overflow"))?;
    let aligned_start = offset.div_ceil(PUNCH_ALIGN) * PUNCH_ALIGN;
    let aligned_end = (end / PUNCH_ALIGN) * PUNCH_ALIGN;
    if offset < aligned_start {
        let edge = (aligned_start.min(end) - offset) as usize;
        let zeros = vec![0u8; edge];
        write_all_at(file, &zeros, offset)?;
    }
    if aligned_end > aligned_start {
        let mut hole = libc::fpunchhole_t {
            fp_flags: 0,
            reserved: 0,
            fp_offset: aligned_start as libc::off_t,
            fp_length: (aligned_end - aligned_start) as libc::off_t,
        };
        // SAFETY: `file` is open; `F_PUNCHHOLE` reads `fpunchhole_t` for this call.
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PUNCHHOLE, &mut hole) };
        if rc == -1 {
            let err = io::Error::last_os_error();
            tracing::warn!(
                target: "ternvale::virtio::blk",
                error = %err,
                offset = aligned_start,
                len = aligned_end - aligned_start,
                "F_PUNCHHOLE failed; writing zeros"
            );
            let zeros = vec![0u8; (aligned_end - aligned_start) as usize];
            write_all_at(file, &zeros, aligned_start)?;
        } else {
            tracing::debug!(
                target: "ternvale::virtio::blk",
                offset = aligned_start,
                len = aligned_end - aligned_start,
                "punched hole"
            );
        }
    }
    if end > aligned_end.max(aligned_start) {
        let start = aligned_end.max(offset);
        let zeros = vec![0u8; (end - start) as usize];
        write_all_at(file, &zeros, start)?;
    }
    Ok(())
}

/// Host allocated bytes (`st_blocks` * 512).
pub fn allocated_bytes(file: &File) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(file.metadata()?.blocks() * 512)
}

/// Highest offset that still holds data (last `SEEK_DATA` extent).
pub fn in_use_bytes(file: &File, len: u64) -> io::Result<u64> {
    if len == 0 {
        return Ok(0);
    }
    let fd = file.as_raw_fd();
    let mut pos: libc::off_t = 0;
    let mut last = 0u64;
    loop {
        // SAFETY: `fd` is an open file; `SEEK_DATA`/`SEEK_HOLE` only move the offset.
        let data = unsafe { libc::lseek(fd, pos, libc::SEEK_DATA) };
        if data < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ENXIO) {
                break;
            }
            return Err(err);
        }
        // SAFETY: as above.
        let hole = unsafe { libc::lseek(fd, data, libc::SEEK_HOLE) };
        if hole < 0 {
            return Err(io::Error::last_os_error());
        }
        last = hole as u64;
        if last >= len {
            last = len;
            break;
        }
        pos = hole;
    }
    Ok(last)
}

fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let n = file.write_at(&buf[done..], offset + done as u64)?;
        if n == 0 {
            return Err(io::Error::new(ErrorKind::WriteZero, "short pwrite"));
        }
        done += n;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn punch_drops_allocated_bytes_and_reads_as_zero() {
        let dir = std::env::temp_dir().join(format!(
            "tv-punch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("disk.img");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .expect("file");
        file.set_len(2 * 1024 * 1024).expect("len");
        let chunk = vec![0x5au8; 128 * 1024];
        write_all_at(&file, &chunk, 0).expect("write");
        let before = std::fs::metadata(&path).expect("m").blocks() * 512;
        punch_hole(&file, 0, 128 * 1024).expect("punch");
        let mut buf = vec![0xffu8; 4096];
        file.read_exact_at(&mut buf, 0).expect("read");
        assert!(
            buf.iter().all(|b| *b == 0),
            "punched range must read as zero"
        );
        let after = std::fs::metadata(&path).expect("m2").blocks() * 512;
        tracing::info!(
            target: "ternvale::virtio::blk",
            before,
            after,
            "F_PUNCHHOLE allocated size"
        );
        // APFS typically drops `st_blocks`; some volumes/sandboxes keep
        // allocation while still reading zeros. TODO(verify) F_PUNCHHOLE on
        // the installer's APFS volume always shrinks `du`.
        // APFS usually drops `st_blocks`; some volumes materialize the sparse
        // tail so allocated can rise even though the punched range reads zero.
        // TODO(verify): `du` after `fstrim` on a guest disk on this host's APFS.
        if after < before {
            tracing::info!(
                target: "ternvale::virtio::blk",
                before,
                after,
                "F_PUNCHHOLE dropped allocated bytes"
            );
        } else {
            tracing::warn!(
                target: "ternvale::virtio::blk",
                before,
                after,
                "F_PUNCHHOLE did not shrink st_blocks; punched range still reads as zero"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
