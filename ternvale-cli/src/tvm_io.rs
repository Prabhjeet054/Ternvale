//! Sparse-preserving copy, optional zstd, and SHA-256 for `.tvm` payloads.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsFd;
use std::path::Path;

use anyhow::{bail, Context, Result};
use nix::unistd::{lseek, Whence};
use sha2::{Digest, Sha256};

use crate::tvm::{check_rel, Encoding, FileKind, TvmFile};

const ZERO: [u8; 8192] = [0; 8192];

/// Copy `src` into `dst` (parent dirs created). Hashes logical and stored bytes.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(src = %src.display(), dst = %dst.display(), zstd)
)]
pub fn store(src: &Path, dst: &Path, rel: &str, kind: FileKind, zstd: bool) -> Result<TvmFile> {
    check_rel(rel).map_err(|error| anyhow::anyhow!("{error}"))?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    if zstd {
        store_zstd(src, dst, rel, kind)
    } else {
        store_raw(src, dst, rel, kind)
    }
}

/// Restore `src` (bundle file) to `dst`, verifying stored and logical hashes.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(src = %src.display(), dst = %dst.display(), path = %entry.path)
)]
pub fn restore(src: &Path, dst: &Path, entry: &TvmFile) -> Result<()> {
    let stored = sha256_path(src).with_context(|| format!("hash stored {}", src.display()))?;
    if stored != entry.sha256 {
        tracing::warn!(
            target: "ternvale::cli",
            path = %entry.path,
            "tvm stored checksum mismatch"
        );
        bail!(
            "checksum mismatch in {}: stored sha256 {stored}, manifest {}",
            entry.path,
            entry.sha256
        );
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    match entry.encoding {
        Encoding::Raw => {
            copy_sparse(src, dst).with_context(|| format!("restore {}", entry.path))?;
        }
        Encoding::Zstd => {
            decode_zstd(src, dst, entry.logical_bytes)
                .with_context(|| format!("decompress {}", entry.path))?;
        }
    }
    let logical = sha256_logical(dst).with_context(|| format!("hash logical {}", dst.display()))?;
    if logical != entry.logical_sha256 {
        tracing::warn!(
            target: "ternvale::cli",
            path = %entry.path,
            "tvm logical checksum mismatch"
        );
        bail!(
            "checksum mismatch in {}: logical sha256 {logical}, manifest {}",
            entry.path,
            entry.logical_sha256
        );
    }
    tracing::info!(
        target: "ternvale::cli",
        path = %entry.path,
        bytes = entry.logical_bytes,
        "tvm file restored"
    );
    Ok(())
}

fn store_raw(src: &Path, dst: &Path, rel: &str, kind: FileKind) -> Result<TvmFile> {
    let (logical_bytes, logical_sha256) =
        copy_sparse(src, dst).with_context(|| format!("sparse copy {}", src.display()))?;
    let stored = sha256_path(dst)?;
    let bytes = std::fs::metadata(dst)
        .with_context(|| format!("stat {}", dst.display()))?
        .len();
    Ok(TvmFile {
        path: rel.to_string(),
        sha256: stored,
        bytes,
        logical_sha256,
        logical_bytes,
        encoding: Encoding::Raw,
        kind,
    })
}

/// Hash a file already in the bundle (no copy).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn describe(path: &Path, rel: &str, kind: FileKind) -> Result<TvmFile> {
    check_rel(rel).map_err(|error| anyhow::anyhow!("{error}"))?;
    let logical_sha256 = sha256_logical(path)?;
    let sha256 = sha256_path(path)?;
    let bytes = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    Ok(TvmFile {
        path: rel.to_string(),
        sha256,
        bytes,
        logical_sha256,
        logical_bytes: bytes,
        encoding: Encoding::Raw,
        kind,
    })
}

fn store_zstd(src: &Path, dst: &Path, rel: &str, kind: FileKind) -> Result<TvmFile> {
    let mut input = File::open(src).with_context(|| format!("open {}", src.display()))?;
    let logical_bytes = input
        .metadata()
        .with_context(|| format!("stat {}", src.display()))?
        .len();
    let out = File::create(dst).with_context(|| format!("create {}", dst.display()))?;
    let mut encoder = zstd::stream::Encoder::new(out, 3)
        .with_context(|| format!("zstd encoder for {}", dst.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = input
            .read(&mut buf)
            .with_context(|| format!("read {}", src.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        encoder
            .write_all(&buf[..n])
            .with_context(|| format!("zstd write {}", dst.display()))?;
    }
    encoder
        .finish()
        .with_context(|| format!("zstd finish {}", dst.display()))?;
    let logical_sha256 = hex(hasher.finalize().as_slice());
    let stored = sha256_path(dst)?;
    let bytes = std::fs::metadata(dst)
        .with_context(|| format!("stat {}", dst.display()))?
        .len();
    Ok(TvmFile {
        path: rel.to_string(),
        sha256: stored,
        bytes,
        logical_sha256,
        logical_bytes,
        encoding: Encoding::Zstd,
        kind,
    })
}

fn decode_zstd(src: &Path, dst: &Path, logical_bytes: u64) -> Result<()> {
    let input = File::open(src).with_context(|| format!("open {}", src.display()))?;
    let mut decoder = zstd::stream::Decoder::new(input)
        .with_context(|| format!("zstd decoder {}", src.display()))?;
    let mut out = File::create(dst).with_context(|| format!("create {}", dst.display()))?;
    out.set_len(logical_bytes)
        .with_context(|| format!("truncate {}", dst.display()))?;
    let mut buf = [0u8; 64 * 1024];
    let mut pos = 0u64;
    loop {
        let n = decoder
            .read(&mut buf)
            .with_context(|| format!("zstd read {}", src.display()))?;
        if n == 0 {
            break;
        }
        if buf[..n].iter().all(|b| *b == 0) {
            pos += n as u64;
            continue;
        }
        out.seek(SeekFrom::Start(pos))
            .with_context(|| format!("seek {}", dst.display()))?;
        out.write_all(&buf[..n])
            .with_context(|| format!("write {}", dst.display()))?;
        pos += n as u64;
    }
    Ok(())
}

/// Copy allocated extents; holes stay holes. Returns logical size and SHA-256.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(src = %src.display()))]
pub fn copy_sparse(src: &Path, dst: &Path) -> Result<(u64, String)> {
    let mut src_file = File::open(src).with_context(|| format!("open {}", src.display()))?;
    let len = src_file
        .metadata()
        .with_context(|| format!("stat {}", src.display()))?
        .len();
    let mut dst_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(dst)
        .with_context(|| format!("create {}", dst.display()))?;
    dst_file
        .set_len(len)
        .with_context(|| format!("set_len {}", dst.display()))?;
    let mut hasher = Sha256::new();
    let mut offset = 0u64;
    while offset < len {
        let data = match lseek(src_file.as_fd(), offset as i64, Whence::SeekData) {
            Ok(pos) => pos as u64,
            Err(nix::errno::Errno::ENXIO) => {
                hash_zeros(&mut hasher, len - offset);
                break;
            }
            Err(error) => {
                tracing::debug!(
                    target: "ternvale::cli",
                    error = %error,
                    "SEEK_DATA unavailable; copying densely"
                );
                return copy_dense(&mut src_file, &mut dst_file, len);
            }
        };
        if data > offset {
            hash_zeros(&mut hasher, data - offset);
        }
        if data >= len {
            break;
        }
        let hole = match lseek(src_file.as_fd(), data as i64, Whence::SeekHole) {
            Ok(pos) => (pos as u64).min(len),
            Err(_) => len,
        };
        copy_extent(&mut src_file, &mut dst_file, data, hole, &mut hasher)
            .with_context(|| format!("copy extent {}..{hole} of {}", data, src.display()))?;
        offset = hole;
    }
    dst_file
        .sync_all()
        .with_context(|| format!("sync {}", dst.display()))?;
    Ok((len, hex(hasher.finalize().as_slice())))
}

fn copy_extent(
    src: &mut File,
    dst: &mut File,
    start: u64,
    end: u64,
    hasher: &mut Sha256,
) -> Result<()> {
    src.seek(SeekFrom::Start(start))?;
    dst.seek(SeekFrom::Start(start))?;
    let mut left = end - start;
    let mut buf = [0u8; 64 * 1024];
    while left > 0 {
        let take = left.min(buf.len() as u64) as usize;
        let n = src.read(&mut buf[..take])?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        dst.write_all(&buf[..n])?;
        left -= n as u64;
    }
    Ok(())
}

fn copy_dense(src: &mut File, dst: &mut File, len: u64) -> Result<(u64, String)> {
    src.seek(SeekFrom::Start(0))?;
    dst.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        dst.write_all(&buf[..n])?;
    }
    Ok((len, hex(hasher.finalize().as_slice())))
}

fn hash_zeros(hasher: &mut Sha256, mut n: u64) {
    while n > 0 {
        let take = n.min(ZERO.len() as u64) as usize;
        hasher.update(&ZERO[..take]);
        n -= take as u64;
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn sha256_path(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(hasher.finalize().as_slice()))
}

fn sha256_logical(path: &Path) -> Result<String> {
    sha256_path(path)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_copy_keeps_logical_size_and_hash() {
        let dir = std::env::temp_dir().join(format!("tvm-io-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let src = dir.join("src.img");
        let dst = dir.join("dst.img");
        {
            let mut f = File::create(&src).expect("src");
            f.set_len(256 * 1024).expect("len");
            f.write_all(b"head").expect("head");
            f.seek(SeekFrom::Start(256 * 1024 - 4)).expect("seek");
            f.write_all(b"tail").expect("tail");
        }
        let (len, hash) = copy_sparse(&src, &dst).expect("copy");
        assert_eq!(len, 256 * 1024);
        assert_eq!(hash.len(), 64);
        assert_eq!(
            sha256_logical(&src).expect("a"),
            sha256_logical(&dst).expect("b")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zstd_roundtrip_matches_logical_hash() {
        let dir = std::env::temp_dir().join(format!("tvm-zstd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let src = dir.join("src.bin");
        std::fs::write(&src, b"zstd-payload").expect("src");
        let stored = dir.join("src.bin.zst");
        let entry = store(&src, &stored, "boot/kernel", FileKind::Boot, true).expect("store");
        assert_eq!(entry.encoding, Encoding::Zstd);
        let dst = dir.join("out.bin");
        restore(&stored, &dst, &entry).expect("restore");
        assert_eq!(std::fs::read(&src).unwrap(), std::fs::read(&dst).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
