//! Unit tests for layered COW behavior.

use std::os::unix::fs::FileExt;
use std::path::Path;

use super::{
    DeltaHeader, DeltaLayer, ImageStack, SnapshotError, StackManifest, DEFAULT_BLOCK_SIZE,
    DELTA_HEADER_LEN,
};

const MIB: u64 = 1024 * 1024;

fn temp_dir(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "ternvale-snap-{}-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

fn empty_delta_len(virtual_size: u64) -> u64 {
    let header = DeltaHeader {
        virtual_size,
        block_size: DEFAULT_BLOCK_SIZE,
        bitmap_offset: DELTA_HEADER_LEN as u64,
        bitmap_bytes: 0,
        data_offset: 0,
    };
    let blocks = header.block_count();
    let bitmap_bytes = super::bitmap_bytes_for_blocks(blocks);
    DELTA_HEADER_LEN as u64 + bitmap_bytes
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).expect("metadata").len()
}

fn seed_base(path: &Path, bytes: u64, fill: u8) {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .expect("base");
    file.set_len(bytes).expect("len");
    let block = [fill; 512];
    let mut off = 0u64;
    while off < bytes {
        let chunk = block.len().min((bytes - off) as usize);
        file.write_all_at(&block[..chunk], off).expect("seed");
        off += chunk as u64;
    }
}

#[test]
fn fresh_delta_grows_per_block_not_full_disk() {
    let dir = temp_dir("grow");
    std::fs::create_dir_all(&dir).expect("dir");
    let virtual_size = 8 * MIB;
    let base = dir.join("base.img");
    let delta = dir.join("layer.tdelta");
    let stack_path = dir.join("disk.tstack");
    seed_base(&base, virtual_size, 0x11);
    DeltaLayer::create(&delta, virtual_size).expect("create");
    let empty = file_size(&delta);
    let expected_empty = empty_delta_len(virtual_size);
    assert_eq!(
        empty, expected_empty,
        "new delta should be header+bitmap only"
    );

    let manifest = StackManifest {
        base: std::path::PathBuf::from("base.img"),
        deltas: vec![std::path::PathBuf::from("layer.tdelta")],
    };
    manifest.save(&stack_path).expect("manifest");
    let mut stack = ImageStack::open(&stack_path, false).expect("open");
    let bs = u64::from(DEFAULT_BLOCK_SIZE);
    let one = [0xAAu8; 512];
    stack.write_at(&one, bs).expect("write block 1");
    stack.write_at(&one, bs * 3).expect("write block 3");
    let after = file_size(&delta);
    let max_reasonable = expected_empty + 2 * bs + 4096;
    assert!(
        after <= max_reasonable,
        "delta size {after} should not approach full disk {virtual_size}"
    );
    assert!(
        after > expected_empty + bs,
        "delta should grow after COW allocations"
    );
    assert!(
        after < virtual_size / 2,
        "delta must stay much smaller than virtual_size"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn read_through_falls_back_to_older_layers_and_base() {
    let dir = temp_dir("read");
    std::fs::create_dir_all(&dir).expect("dir");
    let virtual_size = 4 * MIB;
    let base = dir.join("base.img");
    let d1 = dir.join("a.tdelta");
    let d2 = dir.join("b.tdelta");
    let stack_path = dir.join("disk.tstack");
    seed_base(&base, virtual_size, 0x10);
    DeltaLayer::create(&d1, virtual_size).expect("d1");
    DeltaLayer::create(&d2, virtual_size).expect("d2");

    let manifest_one = StackManifest {
        base: std::path::PathBuf::from("base.img"),
        deltas: vec![std::path::PathBuf::from("a.tdelta")],
    };
    manifest_one.save(&stack_path).expect("m1");
    let mut stack = ImageStack::open(&stack_path, false).expect("open");
    let bs = u64::from(DEFAULT_BLOCK_SIZE);
    let patch = [0x22u8; 512];
    stack.write_at(&patch, 0).expect("layer1 block0");
    stack.flush().expect("flush");

    let manifest_two = StackManifest {
        base: std::path::PathBuf::from("base.img"),
        deltas: vec![
            std::path::PathBuf::from("a.tdelta"),
            std::path::PathBuf::from("b.tdelta"),
        ],
    };
    manifest_two.save(&stack_path).expect("m2");
    let mut stack = ImageStack::open(&stack_path, false).expect("reopen");
    let patch2 = [0x33u8; 512];
    stack.write_at(&patch2, bs).expect("layer2 block1");

    let mut buf = [0u8; 512];
    stack.read_at(&mut buf, 0).expect("b0");
    assert_eq!(buf[0], 0x22, "top layer overrides block 0");
    stack.read_at(&mut buf, bs).expect("b1");
    assert_eq!(buf[0], 0x33, "top layer block 1");
    stack.read_at(&mut buf, bs * 2).expect("b2 untouched");
    assert_eq!(buf[0], 0x10, "base visible through stack");

    let ro = ImageStack::open(&stack_path, true).expect("ro");
    ro.read_at(&mut buf, bs * 2).expect("ro base");
    assert_eq!(buf[0], 0x10);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn corrupt_delta_magic_is_rejected() {
    let dir = temp_dir("magic");
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("bad.tdelta");
    let mut bytes = [0u8; DELTA_HEADER_LEN];
    bytes[0..4].copy_from_slice(b"NOPE");
    std::fs::write(&path, bytes).expect("write");
    let err = DeltaLayer::open(&path, false).expect_err("open");
    match err {
        SnapshotError::BadMagic {
            expected,
            path: Some(p),
        } => {
            assert_eq!(expected, "TVDS");
            assert_eq!(p, path);
        }
        other => panic!("expected BadMagic with path, got {other:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}
