//! Step 110 host analogs: I-beam overlay vs scanout, cursor while a frame is
//! in flight, capture hint + relative motion + escape. No live guest window.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::input_tests::Recorder;
use super::tests::{display, session_input, update, FakeViewer};
use super::*;
use crate::present_protocol::{
    FrameHeader, ViewerInput, ViewerMessage, KIND_CAPTURE_HINT, KIND_CURSOR, KIND_FRAME,
};
use crate::resource::BYTES_PER_PIXEL;
use crate::scanout::CursorOverlay;

/// 8×16 I-beam: one-pixel column in the middle, hotspot at (4, 8).
fn ibeam() -> CursorOverlay {
    let (width, height) = (8u32, 16u32);
    let mut pixels = vec![0u8; (width * height * BYTES_PER_PIXEL) as usize];
    let stride = (width * BYTES_PER_PIXEL) as usize;
    for row in 0..height as usize {
        let at = row * stride + 4 * BYTES_PER_PIXEL as usize;
        pixels[at..at + 4].copy_from_slice(&[0x00, 0x00, 0x00, 0xff]);
    }
    CursorOverlay {
        scanout: 0,
        x: 40,
        y: 80,
        hot_x: 4,
        hot_y: 8,
        width,
        height,
        format: 1,
        pixels,
        visible: true,
        sequence: 1,
    }
}

struct RelativeWanted {
    rec: Recorder,
    relative: AtomicBool,
}

impl InputTarget for RelativeWanted {
    fn deliver(&self, input: ViewerInput) {
        self.rec.deliver(input);
    }

    fn relative_wanted(&self) -> bool {
        self.relative.load(Ordering::Relaxed)
    }
}

/// Scanout pixels stay the fill colour; the I-beam lives only on the overlay.
#[test]
fn ibeam_overlay_is_not_composited_into_the_scanout() {
    let display = display();
    update(&display, 0x11, 1);
    display.set_cursor(ibeam());
    let frame = display.frame(0).expect("scanout");
    assert!(
        frame.pixels.iter().all(|&b| b == 0x11),
        "software cursor would have painted the I-beam into the framebuffer"
    );
    let overlay = display.cursor();
    assert!(overlay.visible);
    assert_eq!((overlay.width, overlay.height), (8, 16));
    assert_eq!((overlay.hot_x, overlay.hot_y), (4, 8));
    let mid = 8 * 8 * 4 + 4 * 4;
    assert_eq!(&overlay.pixels[mid..mid + 4], &[0x00, 0x00, 0x00, 0xff]);
}

/// Overlay is written on the wire even while the previous frame is unacked
/// (the host cursor does not wait a full present).
#[test]
fn cursor_overlay_is_sent_while_a_frame_is_in_flight() {
    let display = display();
    update(&display, 0xaa, 1);
    let shared = display.clone();
    let lag_us = Arc::new(AtomicU64::new(u64::MAX));
    let lag = Arc::clone(&lag_us);
    let (result, logs, _) = session_input(
        PresentConfig {
            fps: 120,
            ..PresentConfig::default()
        },
        &display,
        None,
        move |stream| {
            let mut viewer = FakeViewer::hello(stream);
            let (kind, header, pixels) = viewer.read_any();
            assert_eq!(kind, KIND_FRAME, "first message is the scanout");
            assert!(pixels.iter().all(|&b| b == 0xaa));
            let started = Instant::now();
            shared.set_cursor(ibeam());
            let mut saw_cursor = false;
            for _ in 0..8 {
                let (kind, cursor, body) = viewer.read_any();
                if kind == KIND_CURSOR {
                    let us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
                    lag.store(us, Ordering::Relaxed);
                    assert_eq!(cursor.width, 8);
                    assert_eq!(cursor.height, 16);
                    assert_eq!(cursor.damage_x, 4, "hotspot x");
                    assert_eq!(cursor.damage_y, 8, "hotspot y");
                    assert_eq!(cursor.damage_w, 40);
                    assert_eq!(cursor.damage_h, 80);
                    assert_eq!(body.len(), 8 * 16 * 4);
                    saw_cursor = true;
                    break;
                }
                assert_eq!(kind, KIND_FRAME, "only frames until the overlay");
            }
            assert!(saw_cursor, "KIND_CURSOR arrived without a presented ack");
            viewer.ack(&header);
            std::thread::sleep(Duration::from_millis(20));
        },
    );
    assert_eq!(result, Ok("viewer closed the connection"));
    let us = lag_us.load(Ordering::Relaxed);
    assert_ne!(us, u64::MAX, "KIND_CURSOR was timed");
    tracing::info!(
        target: "ternvale::gpu",
        cursor_after_set_us = us,
        "I-beam overlay analog: KIND_CURSOR while frame in flight"
    );
    assert!(
        us < 500_000,
        "overlay waited {us} µs (should not wait for present)"
    );
    assert!(logs.contains("cursor overlay sent"), "{logs}");
}

/// Agent-style relative hint, relative motion, then Capture off (Esc analog).
#[test]
fn capture_hint_relative_motion_and_escape_release() {
    let display = display();
    update(&display, 0x22, 1);
    let target = RelativeWanted {
        rec: Recorder::default(),
        relative: AtomicBool::new(true),
    };
    let (result, logs, stats) = session_input(
        PresentConfig {
            fps: 120,
            ..PresentConfig::default()
        },
        &display,
        Some(&target),
        move |stream| {
            let mut viewer = FakeViewer::hello(stream);
            let mut hinted = false;
            let mut framed: Option<FrameHeader> = None;
            for _ in 0..12 {
                let (kind, header, _) = viewer.read_any();
                if kind == KIND_CAPTURE_HINT {
                    assert_eq!(header.flags & 1, 1, "relative recommended");
                    hinted = true;
                } else if kind == KIND_FRAME {
                    framed = Some(header);
                }
                if hinted && framed.is_some() {
                    break;
                }
            }
            assert!(hinted, "KIND_CAPTURE_HINT for a game / relative agent");
            let header = framed.expect("scanout");
            for (dx, dy) in [(3, -1), (8, 2), (-4, 0)] {
                viewer.send(ViewerMessage::Input(ViewerInput::RelMotion { dx, dy }));
            }
            viewer.send(ViewerMessage::Input(ViewerInput::Capture { on: false }));
            viewer.ack(&header);
            std::thread::sleep(Duration::from_millis(30));
        },
    );
    assert_eq!(result, Ok("viewer closed the connection"));
    let got = target.rec.0.lock().expect("rec").clone();
    assert_eq!(
        got,
        [
            ViewerInput::RelMotion { dx: 3, dy: -1 },
            ViewerInput::RelMotion { dx: 8, dy: 2 },
            ViewerInput::RelMotion { dx: -4, dy: 0 },
            ViewerInput::Capture { on: false },
        ]
    );
    assert_eq!(stats.inputs.load(Ordering::Relaxed), 4);
    assert!(logs.contains("capture hint sent"), "{logs}");
}
