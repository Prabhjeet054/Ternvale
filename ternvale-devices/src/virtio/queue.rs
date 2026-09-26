//! Split virtqueue: descriptor table, available ring, and used ring.
//!
//! Guest-controlled indices, lengths, and addresses are checked before use.
//! A bad value logs a warning and marks the queue broken. Later operations do nothing.

use ternvale_vmm::GuestMemory;

const NEXT: u16 = 1;
const WRITE: u16 = 2;
const INDIRECT: u16 = 4;
const NO_INTERRUPT: u16 = 1;

/// One guest buffer in a descriptor chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buffer {
    /// Guest physical address.
    pub addr: u64,
    /// Length in bytes. Zero is a valid empty buffer.
    pub len: u32,
}

/// One available chain. Readable buffers come before writable ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    /// Descriptor index published in the available ring.
    pub head: u16,
    /// Device-readable buffers.
    pub readable: Vec<Buffer>,
    /// Device-writable buffers.
    pub writable: Vec<Buffer>,
}

/// A split virtqueue bound to guest addresses.
pub struct SplitQueue {
    size: u16,
    desc: u64,
    avail: u64,
    used: u64,
    last_avail: u16,
    used_idx: u16,
    broken: bool,
    indirect: bool,
    event_idx: bool,
}

impl SplitQueue {
    /// Bind a queue. `size` must be a non-zero power of two.
    ///
    /// `indirect` and `event_idx` follow `VIRTIO_F_INDIRECT_DESC` and
    /// `VIRTIO_F_EVENT_IDX` after feature negotiation.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::queue",
        skip_all,
        fields(size, desc = format!("{desc:#x}"), avail = format!("{avail:#x}"), used = format!("{used:#x}"))
    )]
    pub fn new(
        size: u16,
        desc: u64,
        avail: u64,
        used: u64,
        indirect: bool,
        event_idx: bool,
    ) -> Self {
        let mut queue = Self {
            size,
            desc,
            avail,
            used,
            last_avail: 0,
            used_idx: 0,
            broken: false,
            indirect,
            event_idx,
        };
        if size == 0 || !size.is_power_of_two() {
            queue.break_queue("queue size is not a power of two");
        } else if desc % 16 != 0 || avail % 2 != 0 || used % 4 != 0 {
            queue.break_queue("queue address is misaligned");
        }
        queue
    }

    /// The queue rejected a guest-controlled value.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::queue", skip_all)]
    pub fn is_broken(&self) -> bool {
        self.broken
    }

    /// Chains published since the last call, in available-ring order.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::queue", skip_all)]
    pub fn chains<'a>(&'a mut self, mem: &'a GuestMemory) -> Chains<'a> {
        Chains { queue: self, mem }
    }

    /// Record a completed chain. `head` is the index from [`Chain::head`].
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::queue",
        skip(self, mem),
        fields(head, len)
    )]
    pub fn add_used(&mut self, mem: &mut GuestMemory, head: u16, len: u32) -> bool {
        if self.broken {
            tracing::trace!(target: "ternvale::virtio::queue", "used write skipped; queue is broken");
            return false;
        }
        if head >= self.size {
            self.break_queue("used head is outside the queue");
            return false;
        }
        let slot = u64::from(self.used_idx % self.size);
        let Some(elem) = self.offset(self.used, 4 + slot * 8) else {
            return false;
        };
        let Some(len_at) = self.offset(elem, 4) else {
            return false;
        };
        if mem.write_u32(elem, u32::from(head)).is_err() || mem.write_u32(len_at, len).is_err() {
            self.break_queue("used ring is outside guest memory");
            return false;
        }
        let next = self.used_idx.wrapping_add(1);
        let Some(idx_at) = self.offset(self.used, 2) else {
            return false;
        };
        if mem.write_u16(idx_at, next).is_err() {
            self.break_queue("used index is outside guest memory");
            return false;
        }
        self.used_idx = next;
        tracing::trace!(
            target: "ternvale::virtio::queue",
            head,
            len,
            used_idx = next,
            "virtqueue add used"
        );
        true
    }

    /// Whether the driver asked to be told about the latest used element.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::queue", skip_all)]
    pub fn notification_needed(&mut self, mem: &GuestMemory) -> bool {
        if self.broken {
            return false;
        }
        if self.event_idx {
            let Some(event_at) = self.offset(self.avail, 4 + u64::from(self.size) * 2) else {
                return false;
            };
            let Ok(used_event) = mem.read_u16(event_at) else {
                self.break_queue("used event index is outside guest memory");
                return false;
            };
            let new = self.used_idx;
            let old = new.wrapping_sub(1);
            return u16::wrapping_sub(new, used_event.wrapping_add(1))
                < u16::wrapping_sub(new, old);
        }
        mem.read_u16(self.avail)
            .is_ok_and(|flags| flags & NO_INTERRUPT == 0)
    }

    fn pop(&mut self, mem: &GuestMemory) -> Option<Chain> {
        if self.broken {
            return None;
        }
        let idx_at = self.offset(self.avail, 2)?;
        let Ok(idx) = mem.read_u16(idx_at) else {
            self.break_queue("available index is outside guest memory");
            return None;
        };
        if self.last_avail == idx {
            return None;
        }
        let slot = u64::from(self.last_avail % self.size);
        let ring = self.offset(self.avail, 4 + slot * 2)?;
        let Ok(head) = mem.read_u16(ring) else {
            self.break_queue("available ring is outside guest memory");
            return None;
        };
        tracing::trace!(
            target: "ternvale::virtio::queue",
            head,
            avail_idx = idx,
            "virtqueue pop"
        );
        let chain = self.walk(mem, head)?;
        self.last_avail = self.last_avail.wrapping_add(1);
        Some(chain)
    }

    fn walk(&mut self, mem: &GuestMemory, head: u16) -> Option<Chain> {
        if head >= self.size {
            self.break_queue("available head is outside the queue");
            return None;
        }
        let mut readable = Vec::new();
        let mut writable = Vec::new();
        let mut index = head;
        let mut seen = vec![false; usize::from(self.size)];
        let mut writing = false;
        for _ in 0..self.size {
            if seen[usize::from(index)] {
                self.break_queue("descriptor chain loops");
                return None;
            }
            seen[usize::from(index)] = true;
            let desc = self.read_desc(mem, self.desc, index)?;
            if desc.flags & INDIRECT != 0 {
                if !self.indirect || desc.flags & NEXT != 0 {
                    self.break_queue("indirect descriptor is not allowed");
                    return None;
                }
                self.walk_indirect(mem, &desc, &mut readable, &mut writable)?;
                return Some(Chain {
                    head,
                    readable,
                    writable,
                });
            }
            if !self.push_desc(mem, &desc, &mut readable, &mut writable, &mut writing) {
                return None;
            }
            if desc.flags & NEXT == 0 {
                return Some(Chain {
                    head,
                    readable,
                    writable,
                });
            }
            if desc.next >= self.size {
                self.break_queue("descriptor next is outside the queue");
                return None;
            }
            index = desc.next;
        }
        self.break_queue("descriptor chain is longer than the queue");
        None
    }

    fn walk_indirect(
        &mut self,
        mem: &GuestMemory,
        table: &Desc,
        readable: &mut Vec<Buffer>,
        writable: &mut Vec<Buffer>,
    ) -> Option<()> {
        if table.len == 0 || table.len % 16 != 0 {
            self.break_queue("indirect table length is not a multiple of 16");
            return None;
        }
        let count = table.len / 16;
        if count > u32::from(self.size) {
            self.break_queue("indirect table is longer than the queue");
            return None;
        }
        let mut writing = false;
        let mut index = 0u32;
        let mut seen = vec![false; count as usize];
        for _ in 0..count {
            if seen[index as usize] {
                self.break_queue("indirect descriptor chain loops");
                return None;
            }
            seen[index as usize] = true;
            let addr = self.offset(table.addr, u64::from(index) * 16)?;
            let desc = self.read_desc_at(mem, addr)?;
            if desc.flags & INDIRECT != 0 {
                self.break_queue("nested indirect descriptor");
                return None;
            }
            if !self.push_desc(mem, &desc, readable, writable, &mut writing) {
                return None;
            }
            if desc.flags & NEXT == 0 {
                return Some(());
            }
            if u32::from(desc.next) >= count {
                self.break_queue("indirect next is outside the table");
                return None;
            }
            index = u32::from(desc.next);
        }
        self.break_queue("indirect chain is longer than its table");
        None
    }

    fn push_desc(
        &mut self,
        mem: &GuestMemory,
        desc: &Desc,
        readable: &mut Vec<Buffer>,
        writable: &mut Vec<Buffer>,
        writing: &mut bool,
    ) -> bool {
        if desc.flags & WRITE != 0 {
            *writing = true;
            self.push_buf(mem, writable, desc.addr, desc.len)
        } else if *writing {
            self.break_queue("readable descriptor follows a writable one");
            false
        } else {
            self.push_buf(mem, readable, desc.addr, desc.len)
        }
    }

    fn push_buf(&mut self, mem: &GuestMemory, out: &mut Vec<Buffer>, addr: u64, len: u32) -> bool {
        if len > 0 && !span_mapped(mem, addr, len) {
            self.break_queue("descriptor address is outside guest memory");
            return false;
        }
        out.push(Buffer { addr, len });
        true
    }

    fn read_desc(&mut self, mem: &GuestMemory, table: u64, index: u16) -> Option<Desc> {
        let addr = self.offset(table, u64::from(index) * 16)?;
        self.read_desc_at(mem, addr)
    }

    fn read_desc_at(&mut self, mem: &GuestMemory, addr: u64) -> Option<Desc> {
        let end = self.offset(addr, 14)?;
        let addr_val = mem.read_u64(addr);
        let len = mem.read_u32(addr + 8);
        let flags = mem.read_u16(addr + 12);
        let next = mem.read_u16(end);
        match (addr_val, len, flags, next) {
            (Ok(addr), Ok(len), Ok(flags), Ok(next)) => Some(Desc {
                addr,
                len,
                flags,
                next,
            }),
            _ => {
                self.break_queue("descriptor is outside guest memory");
                None
            }
        }
    }

    fn offset(&mut self, base: u64, add: u64) -> Option<u64> {
        base.checked_add(add).or_else(|| {
            self.break_queue("guest address overflows");
            None
        })
    }

    fn break_queue(&mut self, reason: &'static str) {
        if self.broken {
            return;
        }
        self.broken = true;
        tracing::warn!(
            target: "ternvale::virtio::queue",
            reason,
            size = self.size,
            "virtqueue broken"
        );
    }
}

struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// Iterator of available chains. Stops when the ring is empty or broken.
pub struct Chains<'a> {
    queue: &'a mut SplitQueue,
    mem: &'a GuestMemory,
}

impl Iterator for Chains<'_> {
    type Item = Chain;

    fn next(&mut self) -> Option<Self::Item> {
        self.queue.pop(self.mem)
    }
}

fn span_mapped(mem: &GuestMemory, addr: u64, len: u32) -> bool {
    let Some(last) = addr.checked_add(u64::from(len) - 1) else {
        return false;
    };
    mem.read_u8(addr).is_ok() && mem.read_u8(last).is_ok()
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
