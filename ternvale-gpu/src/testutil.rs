//! Test helpers: synthetic guest memory, command encoders, a test pattern,
//! and a one-chain-at-a-time split-queue driver.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_vmm::{GuestMemory, HOST_PAGE_SIZE};

use crate::protocol::{self, Rect};

pub const RAM: u64 = 0x4000_0000;
/// Ring area: descriptors, then available and used rings, per queue.
pub const RINGS: u64 = RAM;
/// Request and response buffers.
pub const REQUEST: u64 = RAM + 0x2000;
pub const RESPONSE: u64 = RAM + 0x3000;
/// Free guest memory for resource backing.
pub const HEAP: u64 = RAM + 0x4000;
pub const PAGES: u64 = 8;
pub const QUEUE_SIZE: u16 = 8;
/// B8G8R8X8, Linux's framebuffer format.
pub const FORMAT_BGRX: u32 = 2;

pub fn memory() -> Arc<Mutex<GuestMemory>> {
    let mut mem = GuestMemory::new().expect("memory");
    mem.add_region(RAM, HOST_PAGE_SIZE * PAGES).expect("region");
    Arc::new(Mutex::new(mem))
}

pub fn hdr(kind: u32, flags: u32, fence_id: u64) -> Vec<u8> {
    let mut out = vec![0u8; protocol::HDR_LEN];
    out[0..4].copy_from_slice(&kind.to_le_bytes());
    out[4..8].copy_from_slice(&flags.to_le_bytes());
    out[8..16].copy_from_slice(&fence_id.to_le_bytes());
    out
}

fn with(kind: u32, words: &[u32]) -> Vec<u8> {
    let mut out = hdr(kind, 0, 0);
    for word in words {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

pub fn get_display_info() -> Vec<u8> {
    hdr(protocol::CMD_GET_DISPLAY_INFO, 0, 0)
}

pub fn create_2d(id: u32, format: u32, width: u32, height: u32) -> Vec<u8> {
    with(
        protocol::CMD_RESOURCE_CREATE_2D,
        &[id, format, width, height],
    )
}

pub fn unref(id: u32) -> Vec<u8> {
    with(protocol::CMD_RESOURCE_UNREF, &[id, 0])
}

pub fn set_scanout(r: Rect, scanout: u32, id: u32) -> Vec<u8> {
    with(
        protocol::CMD_SET_SCANOUT,
        &[r.x, r.y, r.width, r.height, scanout, id],
    )
}

pub fn flush(r: Rect, id: u32) -> Vec<u8> {
    with(
        protocol::CMD_RESOURCE_FLUSH,
        &[r.x, r.y, r.width, r.height, id, 0],
    )
}

pub fn transfer(r: Rect, offset: u64, id: u32) -> Vec<u8> {
    let mut out = with(
        protocol::CMD_TRANSFER_TO_HOST_2D,
        &[r.x, r.y, r.width, r.height],
    );
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

pub fn attach_backing(id: u32, entries: &[(u64, u32)]) -> Vec<u8> {
    let mut out = with(
        protocol::CMD_RESOURCE_ATTACH_BACKING,
        &[id, entries.len() as u32],
    );
    for (addr, len) in entries {
        out.extend_from_slice(&addr.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    out
}

pub fn detach_backing(id: u32) -> Vec<u8> {
    with(protocol::CMD_RESOURCE_DETACH_BACKING, &[id, 0])
}

pub fn rect(x: u32, y: u32, width: u32, height: u32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Response type of an answer.
pub fn kind(response: &[u8]) -> u32 {
    protocol::parse_hdr(response).expect("response header").kind
}

/// The test pattern's pixel (`x`, `y`), BGRX bytes. `tag` tells two patterns apart.
pub fn pattern(x: u32, y: u32, tag: u8) -> [u8; 4] {
    [x as u8, y as u8, (x as u8 ^ y as u8) ^ tag, 0xff]
}

/// A `width` × `height` image of [`pattern`].
pub fn image(width: u32, height: u32, tag: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            out.extend_from_slice(&pattern(x, y, tag));
        }
    }
    out
}

/// Write `bytes` (the resource's layout) across backing `entries`, in order.
pub fn fill_backing(mem: &Arc<Mutex<GuestMemory>>, entries: &[(u64, u32)], bytes: &[u8]) {
    let mut mem = mem.lock().expect("mem");
    let mut at = 0usize;
    for &(addr, len) in entries {
        let take = (len as usize).min(bytes.len() - at);
        mem.write_bytes(addr, &bytes[at..at + take])
            .expect("backing");
        at += take;
    }
}

/// Places one request/response chain at a time on queue `queue`'s rings and
/// waits for the device's used-ring entry.
pub struct Driver {
    pub memory: Arc<Mutex<GuestMemory>>,
    pub queue: u16,
    published: u16,
}

impl Driver {
    pub fn new(memory: Arc<Mutex<GuestMemory>>, queue: u16) -> Self {
        Self {
            memory,
            queue,
            published: 0,
        }
    }

    pub fn desc_addr(&self) -> u64 {
        RINGS + u64::from(self.queue) * 0x1000
    }

    pub fn avail_addr(&self) -> u64 {
        self.desc_addr() + 0x400
    }

    pub fn used_addr(&self) -> u64 {
        self.desc_addr() + 0x800
    }

    /// Publish `request` with a `response_len`-byte writable buffer. The caller kicks.
    pub fn publish(&mut self, request: &[u8], response_len: u32) {
        let mut mem = self.memory.lock().expect("mem");
        let request_at = REQUEST + u64::from(self.queue) * 0x800;
        let response_at = RESPONSE + u64::from(self.queue) * 0x800;
        mem.write_bytes(request_at, request).expect("request");
        mem.write_bytes(response_at, &vec![0xee; response_len as usize])
            .expect("response");
        let desc = self.desc_addr();
        let mut put = |index: u64, addr: u64, len: u32, flags: u16, next: u16| {
            let at = desc + index * 16;
            mem.write_u64(at, addr).expect("addr");
            mem.write_u32(at + 8, len).expect("len");
            mem.write_u16(at + 12, flags).expect("flags");
            mem.write_u16(at + 14, next).expect("next");
        };
        if response_len == 0 {
            put(0, request_at, request.len() as u32, 0, 0);
        } else {
            put(0, request_at, request.len() as u32, 1, 1);
            put(1, response_at, response_len, 2, 0);
        }
        let slot = u64::from(self.published % QUEUE_SIZE);
        mem.write_u16(self.avail_addr() + 4 + slot * 2, 0)
            .expect("ring");
        self.published = self.published.wrapping_add(1);
        mem.write_u16(self.avail_addr() + 2, self.published)
            .expect("idx");
    }

    /// Wait for the used ring to catch up, then return the response bytes.
    pub fn wait_response(&self) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let used = self
                .memory
                .lock()
                .expect("mem")
                .read_u16(self.used_addr() + 2)
                .expect("used idx");
            if used == self.published {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "used ring at {used}, want {}",
                self.published
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let mem = self.memory.lock().expect("mem");
        let slot = u64::from(self.published.wrapping_sub(1) % QUEUE_SIZE);
        let len = mem
            .read_u32(self.used_addr() + 4 + slot * 8 + 4)
            .expect("used len");
        let mut out = vec![0u8; len as usize];
        mem.read_bytes(RESPONSE + u64::from(self.queue) * 0x800, &mut out)
            .expect("response");
        out
    }
}
