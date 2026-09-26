use ternvale_vmm::{GuestMemory, HOST_PAGE_SIZE};

use super::{Buffer, SplitQueue};

const NEXT: u16 = 1;
const WRITE: u16 = 2;
const INDIRECT: u16 = 4;

struct Rings {
    mem: GuestMemory,
    base: u64,
    size: u16,
}

impl Rings {
    fn new(size: u16) -> Self {
        let mut mem = GuestMemory::new().expect("memory");
        let base = 0x4000_0000u64;
        mem.add_region(base, u64::from(HOST_PAGE_SIZE))
            .expect("region");
        Self { mem, base, size }
    }

    fn desc(&self, index: u16) -> u64 {
        self.base + u64::from(index) * 16
    }

    fn avail(&self) -> u64 {
        self.base + 0x400
    }

    fn used(&self) -> u64 {
        self.base + 0x800
    }

    fn queue(&self, indirect: bool, event_idx: bool) -> SplitQueue {
        SplitQueue::new(
            self.size,
            self.base,
            self.avail(),
            self.used(),
            indirect,
            event_idx,
        )
    }

    fn write_desc(&mut self, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let at = self.desc(index);
        self.mem.write_u64(at, addr).expect("addr");
        self.mem.write_u32(at + 8, len).expect("len");
        self.mem.write_u16(at + 12, flags).expect("flags");
        self.mem.write_u16(at + 14, next).expect("next");
    }

    fn publish(&mut self, heads: &[u16]) {
        for (slot, head) in heads.iter().enumerate() {
            let at = self.avail() + 4 + (slot as u64) * 2;
            self.mem.write_u16(at, *head).expect("ring");
        }
        self.mem
            .write_u16(self.avail() + 2, heads.len() as u16)
            .expect("idx");
    }
}

#[test]
fn single_chain_is_completed_in_the_used_ring() {
    let mut rings = Rings::new(4);
    let data = rings.base + 0x100;
    rings.mem.write_u32(data, 0x1122_3344).expect("data");
    rings.write_desc(0, data, 4, 0, 0);
    rings.publish(&[0]);
    let mut queue = rings.queue(false, false);
    let chain = queue.chains(&rings.mem).next().expect("chain");
    assert_eq!(chain.head, 0);
    assert_eq!(chain.readable, vec![Buffer { addr: data, len: 4 }]);
    assert!(chain.writable.is_empty());
    assert!(queue.add_used(&mut rings.mem, chain.head, 4));
    assert_eq!(rings.mem.read_u16(rings.used() + 2).expect("idx"), 1);
    assert_eq!(rings.mem.read_u32(rings.used() + 4).expect("id"), 0);
    assert_eq!(rings.mem.read_u32(rings.used() + 8).expect("len"), 4);
    assert!(queue.notification_needed(&rings.mem));
}

#[test]
fn chained_descriptors_split_readable_and_writable() {
    let mut rings = Rings::new(4);
    rings.write_desc(0, rings.base + 0x100, 2, NEXT, 1);
    rings.write_desc(1, rings.base + 0x200, 8, WRITE, 0);
    rings.publish(&[0]);
    let mut queue = rings.queue(false, false);
    let chain = queue.chains(&rings.mem).next().expect("chain");
    assert_eq!(chain.readable.len(), 1);
    assert_eq!(
        chain.writable,
        vec![Buffer {
            addr: rings.base + 0x200,
            len: 8
        }]
    );
}

#[test]
fn indirect_table_is_walked_when_the_feature_is_on() {
    let mut rings = Rings::new(4);
    let table = rings.base + 0x200;
    rings
        .mem
        .write_u64(table, rings.base + 0x300)
        .expect("iaddr");
    rings.mem.write_u32(table + 8, 4).expect("ilen");
    rings.mem.write_u16(table + 12, NEXT).expect("iflags");
    rings.mem.write_u16(table + 14, 1).expect("inext");
    rings
        .mem
        .write_u64(table + 16, rings.base + 0x310)
        .expect("iaddr2");
    rings.mem.write_u32(table + 24, 1).expect("ilen2");
    rings.mem.write_u16(table + 28, WRITE).expect("iflags2");
    rings.write_desc(0, table, 32, INDIRECT, 0);
    rings.publish(&[0]);
    let mut queue = rings.queue(true, false);
    let chain = queue.chains(&rings.mem).next().expect("chain");
    assert_eq!(chain.readable.len(), 1);
    assert_eq!(chain.writable.len(), 1);
    let mut disabled = rings.queue(false, false);
    assert!(disabled.chains(&rings.mem).next().is_none());
    assert!(disabled.is_broken());
}

#[test]
fn avail_index_wraps_in_sixteen_bits() {
    let mut rings = Rings::new(4);
    rings.write_desc(1, rings.base + 0x100, 1, 0, 0);
    rings.mem.write_u16(rings.avail() + 2, 0).expect("idx");
    let slot = u64::from(u16::MAX % 4);
    rings
        .mem
        .write_u16(rings.avail() + 4 + slot * 2, 1)
        .expect("slot");
    let mut queue = rings.queue(false, false);
    queue.last_avail = 65535;
    let chain = queue.chains(&rings.mem).next().expect("wrapped");
    assert_eq!(chain.head, 1);
    assert!(queue.chains(&rings.mem).next().is_none());
}

#[test]
fn a_descriptor_loop_breaks_the_queue() {
    let mut rings = Rings::new(4);
    rings.write_desc(0, rings.base + 0x100, 1, NEXT, 0);
    rings.publish(&[0]);
    let mut queue = rings.queue(false, false);
    assert!(queue.chains(&rings.mem).next().is_none());
    assert!(queue.is_broken());
    assert!(!queue.add_used(&mut rings.mem, 0, 1));
}

#[test]
fn an_out_of_range_address_breaks_the_queue() {
    let mut rings = Rings::new(4);
    rings.write_desc(0, 0x1000, 8, 0, 0);
    rings.publish(&[0]);
    let mut queue = rings.queue(false, false);
    assert!(queue.chains(&rings.mem).next().is_none());
    assert!(queue.is_broken());
}

#[test]
fn a_zero_length_descriptor_is_an_empty_buffer() {
    let mut rings = Rings::new(4);
    rings.write_desc(0, 0, 0, 0, 0);
    rings.publish(&[0]);
    let mut queue = rings.queue(false, false);
    let chain = queue.chains(&rings.mem).next().expect("chain");
    assert_eq!(chain.readable, vec![Buffer { addr: 0, len: 0 }]);
    assert!(!queue.is_broken());
}

#[test]
fn event_idx_suppresses_the_interrupt_until_the_index_passes() {
    let mut rings = Rings::new(4);
    rings.write_desc(0, rings.base + 0x100, 1, 0, 0);
    rings.publish(&[0]);
    let event = rings.avail() + 4 + 4 * 2;
    rings.mem.write_u16(event, 1).expect("used_event");
    let mut queue = rings.queue(false, true);
    let chain = queue.chains(&rings.mem).next().expect("chain");
    assert!(queue.add_used(&mut rings.mem, chain.head, 1));
    assert!(!queue.notification_needed(&rings.mem));
    rings.mem.write_u16(event, 0).expect("used_event");
    assert!(queue.notification_needed(&rings.mem));
}

#[test]
fn used_index_wraps_in_sixteen_bits() {
    let mut rings = Rings::new(4);
    rings.write_desc(2, rings.base + 0x100, 1, 0, 0);
    rings.publish(&[2]);
    let mut queue = rings.queue(false, false);
    queue.used_idx = u16::MAX;
    let chain = queue.chains(&rings.mem).next().expect("chain");
    assert!(queue.add_used(&mut rings.mem, chain.head, 7));
    assert_eq!(rings.mem.read_u16(rings.used() + 2).expect("idx"), 0);
    let slot = u64::from(u16::MAX % 4);
    let elem = rings.used() + 4 + slot * 8;
    assert_eq!(rings.mem.read_u32(elem).expect("id"), 2);
    assert_eq!(rings.mem.read_u32(elem + 4).expect("len"), 7);
}

/// xorshift64. A zero seed stays zero, so the fuzz starts from a fixed non-zero value.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u16(&mut self) -> u16 {
        self.next_u64() as u16
    }
}

#[test]
fn ten_thousand_random_rings_do_not_panic() {
    let _quiet = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::new());
    let mut rings = Rings::new(4);
    let mut rng = Rng(0x717e_5120_20ab);
    let mut bytes = vec![0u8; HOST_PAGE_SIZE as usize];
    for n in 0..10_000 {
        let seed = rng.0;
        for chunk in bytes.chunks_mut(8) {
            chunk.copy_from_slice(&rng.next_u64().to_le_bytes());
        }
        rings
            .mem
            .write_bytes(rings.base, &bytes)
            .expect("fill guest page");
        let size = if n % 2 == 0 { 4 } else { rng.next_u16() };
        let (desc, avail, used) = if n % 4 != 0 {
            (rings.base, rings.avail(), rings.used())
        } else {
            (rng.next_u64(), rng.next_u64(), rng.next_u64())
        };
        let indirect = rng.next_u64() & 1 == 1;
        let event_idx = rng.next_u64() & 1 == 1;
        let head = rng.next_u16();
        let len = rng.next_u32();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut queue = SplitQueue::new(size, desc, avail, used, indirect, event_idx);
            let mut walked = 0u32;
            for chain in queue.chains(&rings.mem) {
                walked = walked.wrapping_add(u32::from(chain.head));
                walked = walked.wrapping_add(chain.readable.len() as u32);
                walked = walked.wrapping_add(chain.writable.len() as u32);
            }
            let wrote = queue.add_used(&mut rings.mem, head, len);
            let needed = queue.notification_needed(&rings.mem);
            std::hint::black_box((walked, wrote, needed, queue.is_broken()));
        }));
        assert!(outcome.is_ok(), "ring {n} panicked from seed {seed:#x}");
    }
}
