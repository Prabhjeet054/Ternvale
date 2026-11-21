//! Step 116 host analogs: 5-snapshot counter chain and a named corrupt link.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

use super::*;

const PAGE: usize = 16 * 1024;

fn tmp(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tv-snap-q-{}-{}-{tag}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("root");
    dir
}

fn payload(
    label: &str,
    ram_size: u64,
    ram: Vec<u8>,
    dirty: Vec<(u64, Vec<u8>)>,
    parent: Option<&str>,
) -> VmSnapshotPayload {
    let mut meta = new_meta("lab", label, 0, ram_size, 0, 1);
    if dirty.is_empty() {
        meta.ram_policy = "full".into();
        meta.ram_sha256 = Some(sha256_hex(&ram));
    } else {
        meta.ram_policy = "incremental".into();
        meta.parent = parent.map(str::to_string);
        meta.dirty_pages = Some(dirty.len() as u64);
        meta.ram_sha256 = Some(sha256_hex(label.as_bytes()));
    }
    VmSnapshotPayload {
        meta,
        gic: vec![],
        ram,
        vcpus_json: "[]".into(),
        devices_json: "{}".into(),
        dirty,
    }
}

fn save_chain(root: &Path, labels: &[&str]) {
    let mut chain = SnapshotChain::empty();
    let mut parent: Option<String> = None;
    for label in labels {
        chain.upsert(ChainNode {
            label: (*label).into(),
            parent: parent.clone(),
            ram_sha256: (*label).into(),
            ram_policy: if parent.is_none() {
                "full".into()
            } else {
                "incremental".into()
            },
        });
        parent = Some((*label).into());
    }
    chain.save(root).expect("chain");
}

/// Step 53 counter analog: five incrementals, each restore yields that tick.
#[test]
fn five_incremental_snapshots_restore_the_counter() {
    let root = tmp("cnt5");
    let ram_size = PAGE as u64;
    let mut ram = vec![0u8; PAGE];
    ram[0..4].copy_from_slice(&0u32.to_le_bytes());
    write_bundle(
        &root.join("s0"),
        &payload("s0", ram_size, ram, vec![], None),
    )
    .expect("s0");
    let labels = ["s0", "s1", "s2", "s3", "s4", "s5"];
    for tick in 1u32..=5 {
        let label = labels[tick as usize];
        let parent = labels[tick as usize - 1];
        let mut page = vec![0u8; PAGE];
        page[0..4].copy_from_slice(&tick.to_le_bytes());
        write_bundle(
            &root.join(label),
            &payload(label, ram_size, vec![], vec![(0u64, page)], Some(parent)),
        )
        .expect(label);
    }
    save_chain(&root, &labels);

    for tick in 1u32..=5 {
        let label = labels[tick as usize];
        let lazy = collapse_chain(&root.join(label), None, 0, ram_size).expect(label);
        let bytes = fs::read(&lazy).expect("ram");
        let got = u32::from_le_bytes(bytes[0..4].try_into().expect("u32"));
        assert_eq!(got, tick, "restore {label}");
        tracing::info!(
            target: "ternvale::snapshot",
            label,
            tick,
            "step 53 counter analog restored"
        );
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn corrupt_link_is_named_in_the_error() {
    let root = tmp("corrupt");
    let ram_size = PAGE as u64;
    let ram = vec![0x11u8; PAGE];
    write_bundle(
        &root.join("s0"),
        &payload("s0", ram_size, ram, vec![], None),
    )
    .expect("s0");
    let page = vec![0x22u8; PAGE];
    write_bundle(
        &root.join("s1"),
        &payload("s1", ram_size, vec![], vec![(0, page.clone())], Some("s0")),
    )
    .expect("s1");
    write_bundle(
        &root.join("s2"),
        &payload("s2", ram_size, vec![], vec![(0, page)], Some("s1")),
    )
    .expect("s2");
    save_chain(&root, &["s0", "s1", "s2"]);
    fs::remove_dir_all(root.join("s1")).expect("break s1");
    let err = collapse_chain(&root.join("s2"), None, 0, ram_size).expect_err("corrupt");
    let text = err.to_string();
    assert!(
        text.contains("`s1`") || text.contains("s1"),
        "error must name the broken link: {text}"
    );
    tracing::info!(target: "ternvale::snapshot", error = %text, "named corrupt chain link");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn eight_gib_sparse_lazy_restore_is_faster_than_full_copy_analog() {
    // 8 GiB guest analog: sparse file + first-touch vs copying a 4 MiB
    // working set (v1 full-dump stand-in). We do not memcpy 8 GiB.
    const EIGHT_GIB: u64 = 8 << 30;
    const WORK: usize = 4 * 1024 * 1024;
    let root = tmp("8g");
    let sparse = root.join("guest.ram.lazy");
    {
        let f = fs::File::create(&sparse).expect("sparse");
        f.set_len(EIGHT_GIB).expect("8g hole");
    }
    let work = vec![0xABu8; WORK];
    let started = Instant::now();
    let _ = compress_ram(&work).expect("zstd full analog");
    let before_create_ms = started.elapsed().as_millis() as u64;

    let dirty = vec![(0u64, vec![0xCDu8; PAGE])];
    let packed = pack_dirty(PAGE as u32, &dirty).expect("pack");
    let started = Instant::now();
    let _ = compress_ram(&packed).expect("zstd dirty");
    let after_create_ms = started.elapsed().as_millis() as u64;

    let started = Instant::now();
    let mut dst = vec![0u8; WORK];
    dst.copy_from_slice(&work);
    let before_restore_ms = started.elapsed().as_millis() as u64;

    let started = Instant::now();
    let mut buf = [0u8; 1];
    let mut file = fs::File::open(&sparse).expect("open 8g");
    file.seek(SeekFrom::Start(0)).expect("seek");
    file.read_exact(&mut buf).expect("fault-in hole");
    let after_restore_ms = started.elapsed().as_millis() as u64;
    assert_eq!(buf[0], 0);
    tracing::info!(
        target: "ternvale::snapshot",
        eight_gib = EIGHT_GIB,
        before_create_ms,
        after_create_ms,
        before_restore_ms,
        after_restore_ms,
        "8 GiB snapshot create/restore analog (sparse lazy vs 4 MiB full copy)"
    );
    assert!(
        after_create_ms <= before_create_ms + 50,
        "incremental+zstd should not be slower than a 4 MiB full compress analog"
    );
    let _ = fs::remove_dir_all(&root);
}
