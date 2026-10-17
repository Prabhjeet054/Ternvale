use super::*;
use crate::testutil::{self, rect, HEAP, RAM};

fn entry(addr: u64, len: u32) -> MemEntry {
    MemEntry { addr, len }
}

#[test]
fn formats_round_trip_and_unknown_ones_are_refused() {
    for (wire, name) in [
        (1, "B8G8R8A8"),
        (2, "B8G8R8X8"),
        (3, "A8R8G8B8"),
        (4, "X8R8G8B8"),
        (67, "R8G8B8A8"),
        (68, "X8B8G8R8"),
        (121, "A8B8G8R8"),
        (134, "R8G8B8X8"),
    ] {
        assert_eq!(
            PixelFormat::from_wire(wire).map(PixelFormat::name),
            Some(name)
        );
    }
    assert_eq!(PixelFormat::from_wire(0), None);
    assert_eq!(PixelFormat::from_wire(5), None);
}

#[test]
fn backing_reads_span_entries_in_list_order() {
    let memory = testutil::memory();
    {
        let mut mem = memory.lock().expect("mem");
        mem.write_bytes(HEAP + 0x1000, &[1, 2, 3, 4]).expect("a");
        mem.write_bytes(HEAP, &[5, 6, 7, 8, 9, 10]).expect("b");
    }
    // Second entry sits below the first: order comes from the list, not addresses.
    let backing = Backing::new(vec![entry(HEAP + 0x1000, 4), entry(HEAP, 6)]);
    assert_eq!(backing.total(), 10);
    let mem = memory.lock().expect("mem");
    let mut out = [0u8; 6];
    backing.read(&mem, 2, &mut out).expect("read");
    assert_eq!(out, [3, 4, 5, 6, 7, 8]);
    let mut all = [0u8; 10];
    backing.read(&mem, 0, &mut all).expect("read");
    assert_eq!(all, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    let mut none = [0u8; 0];
    backing
        .read(&mem, 10, &mut none)
        .expect("empty read at the end");
}

#[test]
fn backing_reads_past_the_end_or_into_holes_fail() {
    let memory = testutil::memory();
    let mem = memory.lock().expect("mem");
    let backing = Backing::new(vec![
        entry(HEAP, 8),
        entry(HEAP + 0x100, 0),
        entry(HEAP + 0x200, 8),
    ]);
    let mut out = [0u8; 4];
    assert_eq!(
        backing.read(&mem, 14, &mut out),
        Err(TransferError::PastBacking {
            start: 14,
            end: 18,
            total: 16
        })
    );
    assert!(matches!(
        backing.read(&mem, u64::MAX, &mut out),
        Err(TransferError::PastBacking { .. })
    ));
    backing
        .read(&mem, 6, &mut out)
        .expect("read across the empty entry");
    let hole = Backing::new(vec![entry(0x1000, 16)]);
    assert_eq!(
        hole.read(&mem, 0, &mut out),
        Err(TransferError::Unmapped { addr: 0x1000 })
    );
}

#[test]
fn first_unmapped_checks_both_ends_and_wrapping() {
    let memory = testutil::memory();
    let mem = memory.lock().expect("mem");
    let end = RAM + testutil::PAGES * ternvale_vmm::HOST_PAGE_SIZE;
    assert_eq!(
        Backing::new(vec![entry(HEAP, 4096), entry(0, 0)]).first_unmapped(&mem),
        None
    );
    assert_eq!(
        Backing::new(vec![entry(HEAP, 16), entry(end - 8, 16)]).first_unmapped(&mem),
        Some(1)
    );
    assert_eq!(
        Backing::new(vec![entry(RAM - 8, 16)]).first_unmapped(&mem),
        Some(0)
    );
    assert_eq!(
        Backing::new(vec![entry(u64::MAX - 4, 16)]).first_unmapped(&mem),
        Some(0)
    );
}

#[test]
fn transfer_copies_a_sub_rectangle_using_the_resource_stride() {
    let memory = testutil::memory();
    let (width, height) = (8, 4);
    let source = testutil::image(width, height, 0x5a);
    let entries = [(HEAP + 0x40, 48), (HEAP + 0x1000, 80)];
    testutil::fill_backing(&memory, &entries, &source);
    let backing = Backing::new(entries.iter().map(|&(a, l)| entry(a, l)).collect());
    let mut resource = Resource::new(PixelFormat::B8g8r8x8, width, height);
    let r = rect(2, 1, 3, 2);
    let offset = u64::from(r.y) * 32 + u64::from(r.x) * 4;
    let mem = memory.lock().expect("mem");
    assert_eq!(resource.transfer_from(&mem, &backing, r, offset), Ok(24));
    for y in 0..height {
        for x in 0..width {
            let at = (y * width + x) as usize * 4;
            let inside = (2..5).contains(&x) && (1..3).contains(&y);
            let want = if inside {
                testutil::pattern(x, y, 0x5a)
            } else {
                [0; 4]
            };
            assert_eq!(resource.pixels[at..at + 4], want, "pixel {x},{y}");
        }
    }
    // The last row would end past the 128-byte backing.
    let late = rect(0, 3, 8, 1);
    assert!(matches!(
        resource.transfer_from(&mem, &backing, late, 3 * 32 + 4),
        Err(TransferError::PastBacking { .. })
    ));
}
