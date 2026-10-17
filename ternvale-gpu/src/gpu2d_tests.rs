use std::sync::{Arc, Mutex};

use ternvale_vmm::GuestMemory;

use super::*;
use crate::protocol::*;
use crate::testutil::{self, rect, FORMAT_BGRX, HEAP, RAM};

const W: u32 = 64;
const H: u32 = 32;
/// The 8 KiB backing of a 64×32 resource, as two 4 KiB pages in reverse address order.
const PAGES: [(u64, u32); 2] = [(HEAP + 0x2000, 4096), (HEAP, 4096)];

struct Fx {
    memory: Arc<Mutex<GuestMemory>>,
    gpu: Gpu2d,
    display: GpuDisplay,
    stats: Arc<GpuStats>,
}

impl Fx {
    fn new(modes: &[(u32, u32)]) -> Self {
        let display = GpuDisplay::new(modes);
        let stats = Arc::new(GpuStats::default());
        Self {
            memory: testutil::memory(),
            gpu: Gpu2d::new(display.clone(), Arc::clone(&stats)),
            display,
            stats,
        }
    }

    fn answer(&mut self, request: &[u8]) -> Vec<u8> {
        let mem = self.memory.lock().expect("mem");
        self.gpu.handle(&mem, request)
    }

    fn send(&mut self, request: &[u8]) -> u32 {
        testutil::kind(&self.answer(request))
    }

    /// Resource 1 (64×32 BGRX) with the pattern `tag` in its backing, transferred.
    fn resource_with_pattern(&mut self, tag: u8) {
        testutil::fill_backing(&self.memory, &PAGES, &testutil::image(W, H, tag));
        assert_eq!(
            self.send(&testutil::create_2d(1, FORMAT_BGRX, W, H)),
            RESP_OK_NODATA
        );
        assert_eq!(
            self.send(&testutil::attach_backing(1, &PAGES)),
            RESP_OK_NODATA
        );
        assert_eq!(
            self.send(&testutil::transfer(rect(0, 0, W, H), 0, 1)),
            RESP_OK_NODATA
        );
    }
}

#[test]
fn create_attach_transfer_scanout_flush_shows_the_test_pattern() {
    let mut fx = Fx::new(&[(W, H)]);
    fx.resource_with_pattern(0x11);
    assert_eq!(
        fx.send(&testutil::set_scanout(rect(0, 0, W, H), 0, 1)),
        RESP_OK_NODATA
    );
    assert_eq!(fx.display.sequence(0), Some(1));
    assert_eq!(
        fx.send(&testutil::flush(rect(0, 0, W, H), 1)),
        RESP_OK_NODATA
    );
    let frame = fx.display.frame(0).expect("frame");
    assert_eq!(
        (frame.width, frame.height, frame.format),
        (W, H, Some(PixelFormat::B8g8r8x8))
    );
    assert_eq!(
        frame.pixels,
        testutil::image(W, H, 0x11),
        "scanout holds the exact pattern"
    );
    assert_eq!(frame.sequence, 2);
    assert_eq!(fx.stats.snapshot(), [5, 0, 1, u64::from(W * H * 4)]);
}

#[test]
fn partial_transfer_and_flush_update_only_the_damaged_rectangle() {
    let mut fx = Fx::new(&[(W, H)]);
    fx.resource_with_pattern(0x11);
    fx.send(&testutil::set_scanout(rect(0, 0, W, H), 0, 1));
    testutil::fill_backing(&fx.memory, &PAGES, &testutil::image(W, H, 0x77));
    let damage = rect(10, 5, 4, 3);
    let offset = u64::from(damage.y * W * 4 + damage.x * 4);
    assert_eq!(
        fx.send(&testutil::transfer(damage, offset, 1)),
        RESP_OK_NODATA
    );
    let before = fx.display.frame(0).expect("frame");
    assert_eq!(
        before.pixel(11, 6),
        Some(testutil::pattern(11, 6, 0x11)),
        "no flush yet"
    );
    assert_eq!(fx.send(&testutil::flush(damage, 1)), RESP_OK_NODATA);
    let after = fx.display.frame(0).expect("frame");
    for y in 0..H {
        for x in 0..W {
            let inside = (10..14).contains(&x) && (5..8).contains(&y);
            let tag = if inside { 0x77 } else { 0x11 };
            assert_eq!(
                after.pixel(x, y),
                Some(testutil::pattern(x, y, tag)),
                "pixel {x},{y}"
            );
        }
    }
}

#[test]
fn a_scanout_can_show_part_of_a_resource() {
    let mut fx = Fx::new(&[(32, 16)]);
    fx.resource_with_pattern(0x22);
    assert_eq!(
        fx.send(&testutil::set_scanout(rect(16, 8, 32, 16), 0, 1)),
        RESP_OK_NODATA
    );
    let frame = fx.display.frame(0).expect("frame");
    assert_eq!((frame.width, frame.height), (32, 16));
    assert_eq!(frame.pixel(0, 0), Some(testutil::pattern(16, 8, 0x22)));
    assert_eq!(frame.pixel(31, 15), Some(testutil::pattern(47, 23, 0x22)));
    // A flush that only partly overlaps the view copies the overlap.
    assert_eq!(
        fx.send(&testutil::flush(rect(0, 0, 20, 10), 1)),
        RESP_OK_NODATA
    );
    assert_eq!(fx.display.sequence(0), Some(2));
    // One wholly outside the view reaches no scanout.
    assert_eq!(
        fx.send(&testutil::flush(rect(0, 0, 8, 8), 1)),
        RESP_OK_NODATA
    );
    assert_eq!(fx.display.sequence(0), Some(2));
    assert_eq!(fx.stats.snapshot()[2], 1);
}

#[test]
fn display_info_reports_the_configured_modes() {
    let mut fx = Fx::new(&[(1280, 800), (1024, 768)]);
    let out = fx.answer(&testutil::get_display_info());
    assert_eq!(
        out,
        display_info(&CtrlHdr::default(), &[(1280, 800), (1024, 768)])
    );
}

#[test]
fn bad_guest_values_get_error_responses() {
    let mut fx = Fx::new(&[(W, H)]);
    let end = RAM + testutil::PAGES * ternvale_vmm::HOST_PAGE_SIZE;
    let cases: Vec<(&str, Vec<u8>, u32)> = vec![
        (
            "id 0",
            testutil::create_2d(0, FORMAT_BGRX, 4, 4),
            RESP_ERR_INVALID_RESOURCE_ID,
        ),
        (
            "unknown format",
            testutil::create_2d(9, 5, 4, 4),
            RESP_ERR_INVALID_PARAMETER,
        ),
        (
            "zero width",
            testutil::create_2d(9, FORMAT_BGRX, 0, 4),
            RESP_ERR_INVALID_PARAMETER,
        ),
        (
            "too tall",
            testutil::create_2d(9, FORMAT_BGRX, 4, MAX_DIMENSION + 1),
            RESP_ERR_INVALID_PARAMETER,
        ),
        (
            "create",
            testutil::create_2d(1, FORMAT_BGRX, W, H),
            RESP_OK_NODATA,
        ),
        (
            "duplicate id",
            testutil::create_2d(1, FORMAT_BGRX, 4, 4),
            RESP_ERR_INVALID_RESOURCE_ID,
        ),
        (
            "attach to nothing",
            testutil::attach_backing(2, &PAGES),
            RESP_ERR_INVALID_RESOURCE_ID,
        ),
        (
            "backing past RAM",
            testutil::attach_backing(1, &[(end - 16, 4096)]),
            RESP_ERR_INVALID_PARAMETER,
        ),
        (
            "transfer without backing",
            testutil::transfer(rect(0, 0, W, H), 0, 1),
            RESP_ERR_UNSPEC,
        ),
        (
            "detach without backing",
            testutil::detach_backing(1),
            RESP_ERR_UNSPEC,
        ),
        (
            "attach",
            testutil::attach_backing(1, &PAGES),
            RESP_OK_NODATA,
        ),
        (
            "attach twice",
            testutil::attach_backing(1, &PAGES),
            RESP_ERR_UNSPEC,
        ),
        (
            "transfer rect outside",
            testutil::transfer(rect(1, 0, W, H), 0, 1),
            RESP_ERR_INVALID_PARAMETER,
        ),
        (
            "transfer past backing",
            testutil::transfer(rect(0, 0, W, H), 4, 1),
            RESP_ERR_INVALID_PARAMETER,
        ),
        (
            "no such scanout",
            testutil::set_scanout(rect(0, 0, W, H), 1, 1),
            RESP_ERR_INVALID_SCANOUT_ID,
        ),
        (
            "scanout rect outside",
            testutil::set_scanout(rect(0, 0, W + 1, H), 0, 1),
            RESP_ERR_INVALID_PARAMETER,
        ),
        (
            "scanout of nothing",
            testutil::set_scanout(rect(0, 0, W, H), 0, 3),
            RESP_ERR_INVALID_RESOURCE_ID,
        ),
        (
            "flush nothing",
            testutil::flush(rect(0, 0, 1, 1), 3),
            RESP_ERR_INVALID_RESOURCE_ID,
        ),
        (
            "flush rect wraps",
            testutil::flush(rect(u32::MAX, 0, 2, 1), 1),
            RESP_ERR_INVALID_PARAMETER,
        ),
        (
            "unref nothing",
            testutil::unref(3),
            RESP_ERR_INVALID_RESOURCE_ID,
        ),
        ("3D command", testutil::hdr(0x0200, 0, 0), RESP_ERR_UNSPEC),
        ("GET_EDID", testutil::hdr(0x010a, 0, 0), RESP_ERR_UNSPEC),
        (
            "truncated body",
            testutil::create_2d(9, FORMAT_BGRX, 4, 4)[..30].to_vec(),
            RESP_ERR_UNSPEC,
        ),
        ("detach", testutil::detach_backing(1), RESP_OK_NODATA),
    ];
    let mut errors = 0;
    for (what, request, want) in &cases {
        assert_eq!(
            protocol::type_name(fx.send(request)),
            protocol::type_name(*want),
            "{what}"
        );
        errors += u64::from(*want != RESP_OK_NODATA);
    }
    let mut huge = testutil::attach_backing(1, &[]);
    huge[HDR_LEN + 4..HDR_LEN + 8].copy_from_slice(&(MAX_BACKING_ENTRIES + 1).to_le_bytes());
    assert_eq!(
        fx.send(&huge),
        RESP_ERR_INVALID_PARAMETER,
        "entry count cap"
    );
    assert!(
        fx.answer(&[1, 2, 3]).is_empty(),
        "no header, nothing to answer"
    );
    assert_eq!(fx.stats.snapshot()[1], errors + 2);
}

#[test]
fn host_memory_budget_is_enforced_and_freed_by_unref() {
    let mut fx = Fx::new(&[(W, H)]);
    // 16384 × 4096 × 4 bytes is exactly the budget.
    assert_eq!(
        fx.send(&testutil::create_2d(1, FORMAT_BGRX, MAX_DIMENSION, 4096)),
        RESP_OK_NODATA
    );
    assert_eq!(fx.gpu.hostmem(), MAX_HOSTMEM);
    assert_eq!(
        fx.send(&testutil::create_2d(2, FORMAT_BGRX, 1, 1)),
        RESP_ERR_OUT_OF_MEMORY
    );
    assert_eq!(fx.send(&testutil::unref(1)), RESP_OK_NODATA);
    assert_eq!(fx.gpu.hostmem(), 0);
    assert_eq!(
        fx.send(&testutil::create_2d(2, FORMAT_BGRX, 1, 1)),
        RESP_OK_NODATA
    );
}

#[test]
fn unref_reset_and_resource_zero_disable_the_scanout() {
    let mut fx = Fx::new(&[(W, H)]);
    fx.resource_with_pattern(0x33);
    fx.send(&testutil::set_scanout(rect(0, 0, W, H), 0, 1));
    assert_eq!(
        fx.send(&testutil::set_scanout(rect(0, 0, 0, 0), 0, 0)),
        RESP_OK_NODATA
    );
    let off = fx.display.frame(0).expect("frame");
    assert_eq!(
        (off.width, off.format, off.pixels.len(), off.sequence),
        (0, None, 0, 2)
    );

    fx.send(&testutil::set_scanout(rect(0, 0, W, H), 0, 1));
    assert_eq!(fx.send(&testutil::unref(1)), RESP_OK_NODATA);
    assert_eq!(
        fx.display.frame(0).expect("frame").format,
        None,
        "unref blanks the scanout"
    );
    assert_eq!(
        fx.send(&testutil::flush(rect(0, 0, 1, 1), 1)),
        RESP_ERR_INVALID_RESOURCE_ID
    );

    fx.resource_with_pattern(0x33);
    fx.send(&testutil::set_scanout(rect(0, 0, W, H), 0, 1));
    fx.gpu.reset();
    assert_eq!(fx.gpu.hostmem(), 0);
    assert_eq!(
        fx.display.frame(0).expect("frame").format,
        None,
        "reset blanks the scanout"
    );
    assert_eq!(
        fx.send(&testutil::create_2d(1, FORMAT_BGRX, W, H)),
        RESP_OK_NODATA,
        "id free again"
    );
}

#[test]
fn fenced_commands_echo_the_fence() {
    let mut fx = Fx::new(&[(W, H)]);
    let mut request = testutil::create_2d(1, FORMAT_BGRX, W, H);
    request[4..8].copy_from_slice(&FLAG_FENCE.to_le_bytes());
    request[8..16].copy_from_slice(&77u64.to_le_bytes());
    let hdr = parse_hdr(&fx.answer(&request)).expect("hdr");
    assert_eq!(
        (hdr.kind, hdr.flags, hdr.fence_id),
        (RESP_OK_NODATA, FLAG_FENCE, 77)
    );
}

#[test]
fn every_command_logs_at_debug_and_flushes_log_timing_at_trace() {
    let mut fx = Fx::new(&[(W, H)]);
    let ((), log) = crate::test_log::capture(|| {
        fx.send(&testutil::get_display_info());
        fx.resource_with_pattern(0x44);
        fx.send(&testutil::set_scanout(rect(0, 0, W, H), 0, 1));
        fx.send(&testutil::flush(rect(0, 0, W, H), 1));
        fx.send(&testutil::flush(rect(0, 0, 8, 8), 1));
    });
    for name in [
        "GET_DISPLAY_INFO",
        "RESOURCE_CREATE_2D",
        "RESOURCE_ATTACH_BACKING",
        "TRANSFER_TO_HOST_2D",
        "SET_SCANOUT",
        "RESOURCE_FLUSH",
    ] {
        assert!(
            log.lines().any(|l| l.contains("DEBUG")
                && l.contains("virtio-gpu command")
                && l.contains(name)),
            "no DEBUG line for {name}:\n{log}"
        );
    }
    let flushes: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("virtio-gpu frame flush"))
        .collect();
    assert_eq!(flushes.len(), 2, "{log}");
    assert!(flushes
        .iter()
        .all(|l| l.contains("TRACE") && l.contains("copy_us=") && l.contains("bytes=")));
    assert!(
        !flushes[0].contains("since_last_us="),
        "first flush has no interval"
    );
    assert!(
        flushes[1].contains("since_last_us=") && flushes[1].contains("bytes=256"),
        "{}",
        flushes[1]
    );
    assert!(log.contains("virtio-gpu scanout enabled"), "INFO on enable");
}
