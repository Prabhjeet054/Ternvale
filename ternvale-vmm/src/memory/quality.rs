//! Step 116 host analogs: Step 53 counter chain, in-flight blk write, 8 GiB mmap.

use std::time::Instant;

use sha2::{Digest, Sha256};

use super::{GuestMemory, HOST_PAGE_SIZE};

fn mem(pages: u64) -> GuestMemory {
    let mut memory = GuestMemory::new().expect("mem");
    memory
        .add_region(0, HOST_PAGE_SIZE * pages)
        .expect("region");
    memory.set_dirty_tracking(true);
    memory
}

#[test]
fn five_snapshots_restore_step53_counter() {
    let mut memory = mem(2);
    memory.write_u32(0, 0).expect("tick0");
    let mut snaps: Vec<Vec<(u64, Vec<u8>)>> = Vec::new();
    let base = memory.copy_region(0, HOST_PAGE_SIZE * 2).expect("base");
    let _ = memory.take_dirty_pages().expect("clear");
    for tick in 1u32..=5 {
        memory.write_u32(0, tick).expect("tick");
        snaps.push(memory.take_dirty_pages().expect("dirty"));
    }
    for tick in 1u32..=5 {
        memory.write_bytes(0, &base).expect("rewind base");
        for snap in snaps.iter().take(tick as usize) {
            for (gpa, page) in snap {
                memory.write_bytes(*gpa, page).expect("apply");
            }
        }
        let got = memory.read_u32(0).expect("read");
        assert_eq!(got, tick, "counter after restore {tick}");
        tracing::info!(
            target: "ternvale::snapshot",
            tick,
            "step 53 counter analog"
        );
    }
}

#[test]
fn blk_dd_during_snapshot_keeps_disk_and_memory_checksums() {
    // virtio-blk `dd` analog: host_ptr_mut into guest RAM while a host file
    // (disk) is written with the same bytes; snapshot mid-write; restore.
    let mut memory = mem(2);
    let dir = std::env::temp_dir().join(format!("tv-blk-q-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let disk = dir.join("disk.img");
    std::fs::write(&disk, vec![0u8; HOST_PAGE_SIZE as usize * 2]).expect("disk");
    let payload = vec![0x5Au8; 4096];
    let ptr = memory
        .host_ptr_mut(0, payload.len())
        .expect("dma into guest");
    // SAFETY: host_ptr_mut validated the GPA range (dd into guest buffers).
    unsafe {
        std::ptr::copy_nonoverlapping(payload.as_ptr(), ptr, payload.len());
    }
    let mut disk_bytes = std::fs::read(&disk).expect("read disk");
    disk_bytes[..payload.len()].copy_from_slice(&payload);
    std::fs::write(&disk, &disk_bytes).expect("write disk");
    let snap = memory.take_dirty_pages().expect("snap during dd");
    assert!(!snap.is_empty(), "in-flight guest write must be dirty");
    memory.write_bytes(0, &[0u8; 4096]).expect("more dd");
    let mut more = std::fs::read(&disk).expect("disk2");
    more[..4].copy_from_slice(&[1, 2, 3, 4]);
    std::fs::write(&disk, &more).expect("disk2");
    memory
        .write_bytes(0, &[0u8; HOST_PAGE_SIZE as usize * 2])
        .expect("wipe");
    for (gpa, page) in &snap {
        memory.write_bytes(*gpa, page).expect("restore ram");
    }
    std::fs::write(&disk, &disk_bytes).expect("restore disk");
    let mut guest = vec![0u8; 4096];
    memory.read_bytes(0, &mut guest).expect("guest file");
    let disk_restored = std::fs::read(&disk).expect("disk restored");
    let g = Sha256::digest(&guest);
    let d = Sha256::digest(&disk_restored[..4096]);
    assert_eq!(g, d, "memory and disk file checksums must match");
    tracing::info!(
        target: "ternvale::snapshot",
        sha256 = %format!("{:x}", g),
        "dd-during-snapshot analog checksums match"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn eight_gib_lazy_mmap_vs_working_set_copy() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .try_init();
    const EIGHT_GIB: u64 = 8 << 30;
    const WORK: usize = 4 * 1024 * 1024;
    let dir = std::env::temp_dir().join(format!("tv-8g-q-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ram.sparse");
    {
        let f = std::fs::File::create(&path).expect("sparse");
        f.set_len(EIGHT_GIB).expect("8g");
    }
    let work = vec![0xABu8; WORK];
    let started = Instant::now();
    let _ = ternvale_snapshot::compress_ram(&work).expect("full zstd analog");
    let before_create_ms = started.elapsed().as_millis() as u64;
    let packed = ternvale_snapshot::pack_dirty(
        HOST_PAGE_SIZE as u32,
        &[(0u64, vec![0xCDu8; HOST_PAGE_SIZE as usize])],
    )
    .expect("pack");
    let started = Instant::now();
    let _ = ternvale_snapshot::compress_ram(&packed).expect("dirty zstd analog");
    let after_create_ms = started.elapsed().as_millis() as u64;

    let started = Instant::now();
    let mut full = GuestMemory::new().expect("small");
    full.add_region(0, HOST_PAGE_SIZE * 4).expect("ws");
    let blob = vec![0x11u8; (HOST_PAGE_SIZE * 4) as usize];
    full.write_bytes(0, &blob).expect("full restore analog");
    let before_restore_ms = started.elapsed().as_millis() as u64;

    let started = Instant::now();
    let mut lazy = GuestMemory::new().expect("lazy");
    lazy.add_region(0, EIGHT_GIB)
        .expect("8 GiB anonymous overcommit");
    lazy.remap_file_private(0, &path)
        .expect("MAP_PRIVATE 8 GiB sparse");
    let b = lazy.read_u8(0).expect("first touch");
    let after_restore_ms = started.elapsed().as_millis() as u64;
    assert_eq!(b, 0);
    tracing::info!(
        target: "ternvale::snapshot",
        eight_gib = EIGHT_GIB,
        before_create_ms,
        after_create_ms,
        before_restore_ms,
        after_restore_ms,
        "8 GiB lazy MAP_PRIVATE restore analog vs 64 KiB working-set copy"
    );
    assert!(
        after_create_ms <= before_create_ms + 50,
        "incremental create analog should not lag the 4 MiB full dump analog"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
