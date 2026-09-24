use super::host::require_host_alignment;
use super::{GuestMemory, MemoryError, HOST_PAGE_SIZE};

fn memory() -> GuestMemory {
    GuestMemory::new().expect("host page size")
}

#[test]
fn fresh_region_is_zero_and_round_trips_little_endian() {
    let mut memory = memory();
    memory.add_region(0, HOST_PAGE_SIZE).expect("region");
    assert_eq!(memory.read_u64(0).expect("read"), 0);
    memory.write_u32(4, 0x0403_0201).expect("write");
    assert_eq!(memory.read_u8(4).expect("b0"), 0x01);
    assert_eq!(memory.read_u16(5).expect("mid"), 0x0302);
    memory.write_u64(8, 0x0807_0605_0403_0201).expect("u64");
    assert_eq!(memory.read_u64(8).expect("read u64"), 0x0807_0605_0403_0201);
    memory.write_bytes(16, b"tern").expect("bytes");
    let mut buf = [0; 4];
    memory.read_bytes(16, &mut buf).expect("read bytes");
    assert_eq!(&buf, b"tern");
}

#[test]
fn rejects_misaligned_size_and_address() {
    let mut memory = memory();
    let gpa = memory.add_region(0x1000, HOST_PAGE_SIZE).unwrap_err();
    assert!(gpa.to_string().contains("gpa"), "{gpa}");
    let size = memory.add_region(0, 8192).unwrap_err();
    assert!(size.to_string().contains("size"), "{size}");
    let zero = memory.add_region(0, 0).unwrap_err();
    assert!(matches!(zero, MemoryError::MisalignedSize { size: 0, .. }));
    let host = require_host_alignment(0x1000).unwrap_err();
    assert!(matches!(
        host,
        MemoryError::MisalignedHost { addr: 0x1000, .. }
    ));
    let gpa = 0xffff_ffff_ffff_c000;
    let overflow = memory.add_region(gpa, HOST_PAGE_SIZE).unwrap_err();
    assert!(
        matches!(overflow, MemoryError::Overflow { .. }),
        "{overflow}"
    );
}

#[test]
fn rejects_overlap_and_accepts_adjacent_regions() {
    let mut memory = memory();
    memory.add_region(0, HOST_PAGE_SIZE * 2).expect("first");
    let overlap = memory
        .add_region(HOST_PAGE_SIZE, HOST_PAGE_SIZE)
        .unwrap_err();
    assert!(overlap.to_string().contains("overlaps"), "{overlap}");
    memory
        .add_region(HOST_PAGE_SIZE * 2, HOST_PAGE_SIZE)
        .expect("adjacent");
    memory
        .write_u8(HOST_PAGE_SIZE * 2, 0x11)
        .expect("second region");
    assert_eq!(memory.read_u8(0).expect("first byte"), 0);
}

#[test]
fn rejects_out_of_range_and_cross_region_access() {
    let mut memory = memory();
    memory.add_region(0, HOST_PAGE_SIZE).expect("low");
    memory
        .add_region(HOST_PAGE_SIZE, HOST_PAGE_SIZE)
        .expect("high");
    let outside = memory.read_u8(HOST_PAGE_SIZE * 2).unwrap_err();
    assert!(outside.to_string().contains("outside"), "{outside}");
    let outside_write = memory.write_u8(HOST_PAGE_SIZE * 2, 1).unwrap_err();
    assert!(matches!(outside_write, MemoryError::OutOfRange { .. }));
    let edge = memory.read_u16(HOST_PAGE_SIZE - 1).unwrap_err();
    assert!(edge.to_string().contains("crosses"), "{edge}");
    let edge_write = memory.write_u16(HOST_PAGE_SIZE - 1, 0).unwrap_err();
    assert!(matches!(edge_write, MemoryError::CrossRegion { .. }));
}

#[test]
fn rejected_access_is_logged() {
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-mem", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let guard = ternvale_log::init(ternvale_log::LogConfig::new("mem", dir.clone())).expect("log");
    let mut memory = memory();
    memory.add_region(0x4000, HOST_PAGE_SIZE).expect("region");
    let error = memory.read_u8(0).unwrap_err();
    assert!(matches!(error, MemoryError::OutOfRange { .. }));
    let path = guard.log_path().to_path_buf();
    drop(memory);
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    assert!(text.contains("host page size"), "{text}");
    assert!(text.contains("16384"), "{text}");
    assert!(text.contains("rejected guest memory access"), "{text}");
    assert!(text.contains("allocated guest region"), "{text}");
    std::fs::remove_dir_all(&dir).expect("remove log dir");
}

#[test]
#[ignore = "needs-hv"]
fn maps_guest_region_with_hv() {
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-hv", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let previous = std::env::var("TERNVALE_LOG").ok();
    // SAFETY: this ignored test is the only ternvale-vmm test that sets
    // TERNVALE_LOG, and it restores the previous value before returning.
    unsafe { std::env::set_var("TERNVALE_LOG", "trace") };
    let mut config = ternvale_log::LogConfig::new("guestmem", dir.clone());
    config.level = "trace".to_string();
    let guard = ternvale_log::init(config).expect("log");
    let vm = ternvale_hv::Vm::create().expect("vm");
    let mut memory = memory();
    let ram = 64 * 1024 * 1024;
    memory.map(&vm, 0x4000_0000, ram).expect("map");
    memory.write_u8(0x4000_0000, 0xa5).expect("write");
    assert_eq!(memory.read_u8(0x4000_0000).expect("read"), 0xa5);
    let last = 0x4000_0000 + ram - 1;
    memory.write_u8(last, 0x5a).expect("write last");
    assert_eq!(memory.read_u8(last).expect("read last"), 0x5a);
    drop(memory);
    drop(vm);
    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    assert!(
        text.contains("hv_vm_map") && text.contains("result_code=0"),
        "{text}"
    );
    assert!(
        text.contains("hv_vm_unmap") && text.contains("result_code=0"),
        "{text}"
    );
    assert!(text.contains("unmapped guest region"), "{text}");
    assert!(text.contains("host_page_size=16384"), "{text}");
    assert!(text.contains("size=0x4000000"), "{text}");
    let sample = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/ternvale-log-samples/guest-memory.log");
    if let Some(parent) = sample.parent() {
        std::fs::create_dir_all(parent).expect("sample dir");
    }
    std::fs::write(&sample, &text).expect("sample");
    std::fs::remove_dir_all(&dir).expect("remove log dir");
    // SAFETY: same as the set above; this test restores the variable it changed.
    unsafe {
        match previous {
            Some(value) => std::env::set_var("TERNVALE_LOG", value),
            None => std::env::remove_var("TERNVALE_LOG"),
        }
    }
}
