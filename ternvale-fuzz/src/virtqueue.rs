//! Split-queue descriptor walker (`SplitQueue::chains`).

use arbitrary::{Arbitrary, Unstructured};
use ternvale_devices::SplitQueue;

use crate::guest::{Guest, QUEUE_SIZE};
use crate::quiet;

const NEXT: u16 = 1;
const WRITE: u16 = 2;
const INDIRECT: u16 = 4;

/// Structure-aware descriptor table.
#[derive(Debug, Arbitrary)]
pub struct QueueInput {
    /// Enable `VIRTIO_F_INDIRECT_DESC`.
    pub indirect: bool,
    /// Enable `VIRTIO_F_EVENT_IDX`.
    pub event_idx: bool,
    /// Descriptor flags and lengths (capped).
    pub descs: Vec<DescInput>,
    /// Available-ring heads.
    pub heads: Vec<u16>,
}

/// One descriptor.
#[derive(Debug, Arbitrary)]
pub struct DescInput {
    /// Offset from the payload GPA, masked into the scratch page.
    pub rel: u16,
    /// Length (masked).
    pub len: u16,
    /// Guest flags.
    pub flags: u16,
    /// Next index.
    pub next: u16,
}

/// Walk chains built from `data`.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let Some(guest) = Guest::new() else {
        return;
    };
    if let Ok(input) = QueueInput::arbitrary(&mut Unstructured::new(data)) {
        walk(&guest, &input);
        return;
    }
    guest.load_chain(data);
    walk_bound(&guest, data.first().copied().unwrap_or(0) & 4 != 0);
}

fn walk(guest: &Guest, input: &QueueInput) {
    for (i, desc) in input.descs.iter().take(usize::from(QUEUE_SIZE)).enumerate() {
        let addr = guest.payload() + u64::from(desc.rel % 0x800);
        let flags = desc.flags & (NEXT | WRITE | if input.indirect { INDIRECT } else { 0 });
        guest.write_desc(
            i as u16,
            addr,
            u32::from(desc.len % 0x400),
            flags,
            desc.next % QUEUE_SIZE,
        );
    }
    let heads: Vec<u16> = input
        .heads
        .iter()
        .take(8)
        .map(|h| *h % QUEUE_SIZE)
        .collect();
    if heads.is_empty() {
        guest.publish(&[0]);
    } else {
        guest.publish(&heads);
    }
    let mut queue = SplitQueue::new(
        QUEUE_SIZE,
        guest.desc(),
        guest.avail(),
        guest.used(),
        input.indirect,
        input.event_idx,
    );
    consume(guest, &mut queue);
}

fn walk_bound(guest: &Guest, indirect: bool) {
    let mut queue = SplitQueue::new(
        QUEUE_SIZE,
        guest.desc(),
        guest.avail(),
        guest.used(),
        indirect,
        false,
    );
    consume(guest, &mut queue);
}

fn consume(guest: &Guest, queue: &mut SplitQueue) {
    let Ok(mem) = guest.memory.lock() else {
        return;
    };
    let chains: Vec<_> = queue.chains(&mem).take(16).collect();
    drop(mem);
    let Ok(mut mem) = guest.memory.lock() else {
        return;
    };
    for chain in chains {
        let _ = queue.add_used(&mut mem, chain.head, 0);
    }
    let _ = queue.notification_needed(&mem);
}
