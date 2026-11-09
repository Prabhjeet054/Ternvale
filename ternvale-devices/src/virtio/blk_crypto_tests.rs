//! AES-256-XTS on a raw virtio-blk image (no VM). Marker `grep -c`, wrong key,
//! and a fio-shaped microbench vs plaintext.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use ternvale_crypto::{CryptoContext, Secret};

use super::backend::FileBackend;
use super::options::BlkOptions;

const MARKER: &[u8] = b"TERNVALE-PLAINTEXT-MARKER";
const NEEDLE: &str = "TERNVALE-PLAINTEXT-MARKER";

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tv-blk-xts-{}-{}", std::process::id(), tag));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("dir");
    dir
}

fn grep_c(path: &Path, needle: &str) -> u32 {
    let out = Command::new("grep")
        .args(["-a", "-c", "--", needle])
        .arg(path)
        .output()
        .expect("grep");
    if !out.status.success() {
        return 0;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

fn empty_img(path: &Path, bytes: u64) {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(path)
        .expect("create");
    file.set_len(bytes).expect("len");
}

#[test]
fn xts_hides_marker_and_round_trips() {
    let dir = scratch("raw");
    let path = dir.join("disk.img");
    empty_img(&path, 4096);
    let ctx = CryptoContext::from_dek(Secret::new(vec![8u8; 32])).expect("ctx");
    let backend =
        FileBackend::open_crypto(&path, false, BlkOptions::default(), Some(ctx.xts().clone()))
            .expect("open");
    let mut buf = vec![0u8; 512];
    buf[..MARKER.len()].copy_from_slice(MARKER);
    backend.write_at(&buf, 512).expect("write");
    assert_eq!(grep_c(&path, NEEDLE), 0, "marker leaked into ciphertext");
    let mut out = vec![0u8; 512];
    backend.read_at(&mut out, 512).expect("read");
    assert_eq!(&out[..MARKER.len()], MARKER);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn plaintext_control_image_contains_marker() {
    let dir = scratch("plain");
    let path = dir.join("disk.img");
    empty_img(&path, 4096);
    let backend =
        FileBackend::open_crypto(&path, false, BlkOptions::default(), None).expect("open");
    let mut buf = vec![0u8; 512];
    buf[..MARKER.len()].copy_from_slice(MARKER);
    backend.write_at(&buf, 512).expect("write");
    assert!(
        grep_c(&path, NEEDLE) >= 1,
        "unencrypted control image must contain the marker"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn plaintext_open_sees_ciphertext() {
    let dir = scratch("cipher");
    let path = dir.join("disk.img");
    empty_img(&path, 512);
    let ctx = CryptoContext::from_dek(Secret::new(vec![8u8; 32])).expect("ctx");
    let enc =
        FileBackend::open_crypto(&path, false, BlkOptions::default(), Some(ctx.xts().clone()))
            .expect("enc");
    let mut buf = vec![0u8; 512];
    buf[0] = 0x5a;
    enc.write_at(&buf, 0).expect("write");
    let mut disk = [0u8; 512];
    let file = std::fs::File::open(&path).expect("open");
    file.read_exact_at(&mut disk, 0).expect("read raw");
    assert_ne!(disk[0], 0x5a);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn wrong_key_does_not_corrupt_ciphertext() {
    let dir = scratch("wrong");
    let path = dir.join("disk.img");
    empty_img(&path, 1024);
    let good = CryptoContext::from_dek(Secret::new(vec![8u8; 32])).expect("good");
    let enc = FileBackend::open_crypto(
        &path,
        false,
        BlkOptions::default(),
        Some(good.xts().clone()),
    )
    .expect("enc");
    let mut buf = vec![0u8; 512];
    buf[..MARKER.len()].copy_from_slice(MARKER);
    enc.write_at(&buf, 0).expect("write");
    drop(enc);
    let before = fs::read(&path).expect("before");
    let wrap = Secret::new(vec![1u8; 32]);
    let dek = Secret::new(vec![8u8; 32]);
    let blob = ternvale_crypto::wrap_dek(&wrap, &dek).expect("wrap");
    let wrong = Secret::new(vec![9u8; 32]);
    let err = ternvale_crypto::verify_dek(&wrong, &blob).expect_err("auth");
    assert!(err.is_auth(), "{err}");
    let spy = FileBackend::open_crypto(
        &path,
        true,
        BlkOptions::default(),
        Some(CryptoContext::from_dek(wrong).expect("ctx").xts().clone()),
    )
    .expect("wrong open");
    let mut out = vec![0u8; 512];
    spy.read_at(&mut out, 0).expect("read");
    assert_ne!(&out[..MARKER.len()], MARKER);
    drop(spy);
    let after = fs::read(&path).expect("after");
    assert_eq!(before, after, "wrong key must not rewrite the image");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn xts_fio_shaped_microbench() {
    const BYTES: u64 = 8 * 1024 * 1024;
    const RAND: u32 = 2048;
    let dir = scratch("fio");
    let mut lines = Vec::new();
    for (label, xts) in [
        ("plain", None),
        (
            "xts",
            Some(
                CryptoContext::from_dek(Secret::new(vec![8u8; 32]))
                    .expect("ctx")
                    .xts()
                    .clone(),
            ),
        ),
    ] {
        let path = dir.join(format!("{label}.img"));
        empty_img(&path, BYTES);
        let backend =
            FileBackend::open_crypto(&path, false, BlkOptions::default(), xts).expect("open");
        let block = vec![0x5au8; 4096];
        let start = Instant::now();
        let mut off = 0u64;
        while off < BYTES {
            backend.write_at(&block, off).expect("seq write");
            off += 4096;
        }
        let seq_w = start.elapsed();
        let mut read = vec![0u8; 4096];
        let start = Instant::now();
        off = 0;
        while off < BYTES {
            backend.read_at(&mut read, off).expect("seq read");
            off += 4096;
        }
        let seq_r = start.elapsed();
        let start = Instant::now();
        for i in 0..RAND {
            let sector = u64::from(i.wrapping_mul(7919) % (BYTES / 4096) as u32) * 4096;
            backend.write_at(&block, sector).expect("rand write");
        }
        let rand_w = start.elapsed();
        tracing::info!(
            target: "ternvale::crypto",
            label,
            seq_write_ms = seq_w.as_millis() as u64,
            seq_read_ms = seq_r.as_millis() as u64,
            rand_write_ms = rand_w.as_millis() as u64,
            "no-HV AES-256-XTS stand-in (not guest fio)"
        );
        lines.push(format!(
            "{label}\t{}\t{}\t{}",
            seq_w.as_millis(),
            seq_r.as_millis(),
            rand_w.as_millis()
        ));
    }
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../bench-results");
    let _ = fs::create_dir_all(&out);
    let body = format!(
        "mode\tseq_write_ms\tseq_read_ms\trand4k_write_ms\n{}\n",
        lines.join("\n")
    );
    fs::write(out.join("encryption-xts-nohv.tsv"), body).expect("tsv");
    let _ = fs::remove_dir_all(&dir);
}
