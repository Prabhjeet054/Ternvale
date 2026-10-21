//! Command parsing from synthetic guest memory: each request is gathered from
//! a real split-queue chain whose readable descriptors cut it at awkward
//! offsets and sit at descending guest addresses.

use std::sync::Arc;

use ternvale_devices::SplitQueue;
use ternvale_vmm::GuestMemory;

use super::{serve, CONTROL_QUEUE, CURSOR_QUEUE};
use crate::engine::GpuEngine;
use crate::gpu2d::GpuStats;
use crate::protocol::{self, CMD_RESOURCE_ATTACH_BACKING};
use crate::scanout::GpuDisplay;
use crate::test_log;
use crate::testutil::*;

const DESC: u64 = RINGS;
const AVAIL: u64 = RINGS + 0x400;
const USED: u64 = RINGS + 0x800;
/// Top of the request area; pieces are placed downwards from here.
const PIECES_TOP: u64 = REQUEST + 0x1000;
const W: u32 = 16;
const H: u32 = 8;

struct Rig {
    mem: GuestMemory,
    queue: SplitQueue,
    gpu: GpuEngine,
    display: GpuDisplay,
    stats: Arc<GpuStats>,
    published: u16,
}

impl Rig {
    fn new() -> Self {
        let memory = memory();
        let mem = std::mem::replace(
            &mut *memory.lock().expect("mem"),
            GuestMemory::new().expect("empty"),
        );
        let display = GpuDisplay::new(&[(640, 480)]);
        let stats = Arc::new(GpuStats::default());
        Self {
            mem,
            queue: SplitQueue::new(QUEUE_SIZE, DESC, AVAIL, USED, false, false),
            gpu: GpuEngine::new(display.clone(), Arc::clone(&stats)),
            display,
            stats,
            published: 0,
        }
    }

    /// Publish `request` split at `cuts` (one readable descriptor per piece, at
    /// descending addresses) plus a writable response buffer; serve; return the
    /// response bytes and the used length.
    fn submit(&mut self, queue: u16, request: &[u8], cuts: &[usize]) -> Vec<u8> {
        let mut bounds = vec![0];
        bounds.extend(cuts.iter().copied().filter(|&c| c > 0 && c < request.len()));
        bounds.push(request.len());
        let pieces: Vec<&[u8]> = bounds.windows(2).map(|w| &request[w[0]..w[1]]).collect();
        let mut addr = PIECES_TOP;
        let mut index = 0u16;
        for piece in &pieces {
            addr -= (piece.len() as u64 + 0xf) & !0xf;
            self.mem.write_bytes(addr, piece).expect("piece");
            self.put(index, addr, piece.len() as u32, 1, index + 1);
            index += 1;
        }
        let response_at = RESPONSE;
        self.mem
            .write_bytes(response_at, &vec![0xee; protocol::DISPLAY_INFO_LEN])
            .expect("response");
        self.put(index, response_at, protocol::DISPLAY_INFO_LEN as u32, 2, 0);
        let slot = u64::from(self.published % QUEUE_SIZE);
        self.mem.write_u16(AVAIL + 4 + slot * 2, 0).expect("ring");
        self.published = self.published.wrapping_add(1);
        self.mem.write_u16(AVAIL + 2, self.published).expect("idx");

        assert!(serve(queue, &mut self.queue, &mut self.mem, &mut self.gpu));
        assert_eq!(
            self.mem.read_u16(USED + 2).expect("used idx"),
            self.published
        );
        let len = self
            .mem
            .read_u32(USED + 4 + slot * 8 + 4)
            .expect("used len");
        let mut out = vec![0u8; len as usize];
        self.mem
            .read_bytes(response_at, &mut out)
            .expect("read response");
        out
    }

    fn put(&mut self, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let at = DESC + u64::from(index) * 16;
        self.mem.write_u64(at, addr).expect("addr");
        self.mem.write_u32(at + 8, len).expect("len");
        self.mem.write_u16(at + 12, flags).expect("flags");
        self.mem.write_u16(at + 14, next).expect("next");
    }
}

#[test]
fn every_opcode_parses_from_split_guest_memory_descriptors() {
    let mut rig = Rig::new();
    let entries = [(HEAP + 0x1000, 256u32), (HEAP, (W * H * 4) - 256)];
    fill_backing_raw(&mut rig.mem, &entries, &image(W, H, 9));
    let full = rect(0, 0, W, H);
    let attach = attach_backing(7, &entries);
    // (request, descriptor cuts, expected response, decoded command in the DEBUG line)
    let cases: Vec<(Vec<u8>, Vec<usize>, u32, &str)> = vec![
        (get_display_info(), vec![10], protocol::RESP_OK_DISPLAY_INFO, "GetDisplayInfo"),
        (create_2d(7, FORMAT_BGRX, W, H), vec![24, 30], protocol::RESP_OK_NODATA,
            "ResourceCreate2d { resource_id: 7, format: 2, width: 16, height: 8 }"),
        // Linux's layout: the command in one descriptor, the entries in the next.
        (attach.clone(), vec![32], protocol::RESP_OK_NODATA,
            "ResourceAttachBacking { resource_id: 7, entries: [MemEntry { addr: 1073762304, len: 256 }, MemEntry { addr: 1073758208, len: 256 }] }"),
        (transfer(full, 0, 7), vec![3, 41], protocol::RESP_OK_NODATA,
            "TransferToHost2d { rect: Rect { x: 0, y: 0, width: 16, height: 8 }, offset: 0, resource_id: 7 }"),
        (set_scanout(full, 0, 7), vec![24, 44], protocol::RESP_OK_NODATA,
            "SetScanout { rect: Rect { x: 0, y: 0, width: 16, height: 8 }, scanout_id: 0, resource_id: 7 }"),
        (flush(rect(2, 1, 5, 3), 7), vec![1, 2, 25], protocol::RESP_OK_NODATA,
            "ResourceFlush { rect: Rect { x: 2, y: 1, width: 5, height: 3 }, resource_id: 7 }"),
        (detach_backing(7), vec![28], protocol::RESP_OK_NODATA, "ResourceDetachBacking { resource_id: 7 }"),
        (unref(7), vec![12], protocol::RESP_OK_NODATA, "ResourceUnref { resource_id: 7 }"),
    ];
    assert_eq!(kind(&attach), CMD_RESOURCE_ATTACH_BACKING);
    for (request, cuts, want, decoded) in cases {
        let (response, logs) = test_log::capture(|| rig.submit(CONTROL_QUEUE, &request, &cuts));
        assert_eq!(
            kind(&response),
            want,
            "{decoded}: response {:#x}",
            kind(&response)
        );
        assert!(logs.contains(decoded), "{decoded} not in:\n{logs}");
        if want == protocol::RESP_OK_DISPLAY_INFO {
            assert_eq!(response.len(), protocol::DISPLAY_INFO_LEN);
            assert_eq!(
                &response[24..40],
                &[0, 0, 0, 0, 0, 0, 0, 0, 128, 2, 0, 0, 224, 1, 0, 0]
            );
        }
        if decoded.starts_with("ResourceFlush") {
            let frame = rig.display.frame(0).expect("frame");
            assert_eq!((frame.width, frame.height), (W, H));
            assert_eq!(
                frame.pixels,
                image(W, H, 9),
                "backing gathered across both entries"
            );
        }
    }
}

#[test]
fn a_descriptor_outside_guest_ram_breaks_the_queue_before_the_device_parses_it() {
    let mut rig = Rig::new();
    let request = create_2d(3, FORMAT_BGRX, W, H);
    rig.mem
        .write_bytes(PIECES_TOP - 0x10, &request[..24])
        .expect("header");
    rig.put(0, PIECES_TOP - 0x10, 24, 1, 1);
    rig.put(1, 0x10, 16, 1, 2); // outside guest RAM
    rig.put(2, RESPONSE, 24, 2, 0);
    rig.mem.write_u16(AVAIL + 4, 0).expect("ring");
    rig.published = 1;
    rig.mem.write_u16(AVAIL + 2, 1).expect("idx");
    let (completed, logs) =
        test_log::capture(|| serve(CONTROL_QUEUE, &mut rig.queue, &mut rig.mem, &mut rig.gpu));
    assert!(!completed);
    assert!(
        logs.contains("descriptor address is outside guest memory"),
        "{logs}"
    );
    assert!(
        !logs.contains("virtio-gpu command"),
        "the device never saw it:\n{logs}"
    );
    assert_eq!(rig.stats.snapshot(), [0; 4]);
}

#[test]
fn cursor_queue_commands_complete_with_an_empty_response() {
    let mut rig = Rig::new();
    let mut request = hdr(protocol::CMD_UPDATE_CURSOR, 0, 0);
    request.extend_from_slice(&[0u8; 32]);
    let (response, logs) = test_log::capture(|| rig.submit(CURSOR_QUEUE, &request, &[24]));
    assert!(response.is_empty());
    assert!(logs.contains("cursor command ignored"), "{logs}");
}

fn fill_backing_raw(mem: &mut GuestMemory, entries: &[(u64, u32)], bytes: &[u8]) {
    let mut at = 0usize;
    for &(addr, len) in entries {
        let take = (len as usize).min(bytes.len() - at);
        mem.write_bytes(addr, &bytes[at..at + take])
            .expect("backing");
        at += take;
    }
}
