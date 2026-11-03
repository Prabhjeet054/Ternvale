//! Confirm `madvise(MADV_FREE_REUSABLE)` on `hv_vm_map` pages drops `phys_footprint`.

use std::time::Duration;

use ternvale_hv::{phys_footprint, Vm};

use super::{GuestMemory, MemAdvice, HOST_PAGE_SIZE};
use crate::platform::RAM_BASE;

const SPAN: u64 = 32 << 20;

fn touch(memory: &mut GuestMemory, gpa: u64, size: u64) {
    let step = HOST_PAGE_SIZE;
    let mut off = 0u64;
    while off < size {
        memory.write_u8(gpa + off, 0x5a).expect("touch");
        off += step;
    }
}

#[test]
#[ignore = "needs-hv"]
fn madvise_without_unmap_drops_phys_footprint() {
    let vm = Vm::create().expect("hv_vm_create");
    let mut memory = GuestMemory::new().expect("memory");
    memory.map(&vm, RAM_BASE, SPAN).expect("hv_vm_map");
    touch(&mut memory, RAM_BASE, SPAN);
    std::thread::sleep(Duration::from_millis(200));
    let after_touch = phys_footprint().expect("footprint");
    memory
        .advise(RAM_BASE, SPAN as usize, MemAdvice::FreeReusable)
        .expect("madvise reusable");
    std::thread::sleep(Duration::from_millis(200));
    let after_free = phys_footprint().expect("footprint");
    tracing::info!(
        target: "ternvale::mem",
        after_touch,
        after_free,
        span = SPAN,
        "balloon hv footprint"
    );
    assert!(
        after_touch > after_free + (SPAN / 4),
        "phys_footprint did not drop without hv_vm_unmap: touch={after_touch} free={after_free} span={SPAN}"
    );
    memory
        .advise(RAM_BASE, SPAN as usize, MemAdvice::FreeReuse)
        .expect("madvise reuse");
    touch(&mut memory, RAM_BASE, SPAN);
}
