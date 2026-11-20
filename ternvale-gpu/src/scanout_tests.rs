use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use super::*;

#[test]
fn request_host_mode_updates_modes_and_notifies() {
    let display = GpuDisplay::new(&[(1280, 800)]);
    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&calls);
    display.add_host_resize_hook(Box::new(move |scanout, w, h| {
        assert_eq!(scanout, 0);
        assert_eq!((w, h), (1920, 1080));
        seen.fetch_add(1, Ordering::Relaxed);
    }));
    display
        .request_host_mode(0, 1920, 1080)
        .expect("valid mode");
    assert_eq!(display.mode(0), Some((1920, 1080)));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    display.request_host_mode(0, 1920, 1080).expect("same mode");
    assert_eq!(calls.load(Ordering::Relaxed), 1, "no duplicate notify");
}

#[test]
fn extra_host_resize_hook_also_runs() {
    let display = GpuDisplay::new(&[(640, 480)]);
    let a = Arc::new(AtomicU32::new(0));
    let b = Arc::new(AtomicU32::new(0));
    let (aa, bb) = (Arc::clone(&a), Arc::clone(&b));
    display.add_host_resize_hook(Box::new(move |_, _, _| {
        aa.fetch_add(1, Ordering::Relaxed);
    }));
    display.add_host_resize_hook(Box::new(move |_, _, _| {
        bb.fetch_add(1, Ordering::Relaxed);
    }));
    display.request_host_mode(0, 800, 600).expect("resize");
    assert_eq!(a.load(Ordering::Relaxed), 1);
    assert_eq!(b.load(Ordering::Relaxed), 1);
}

#[test]
fn second_scanout_resizes_without_touching_the_first() {
    let display = GpuDisplay::new(&[(1280, 800), (1280, 800)]);
    display.request_host_mode(0, 1920, 1080).expect("scanout 0");
    display.request_host_mode(1, 800, 600).expect("scanout 1");
    assert_eq!(display.mode(0), Some((1920, 1080)));
    assert_eq!(display.mode(1), Some((800, 600)));
}

#[test]
fn request_host_mode_rejects_bad_geometry() {
    let display = GpuDisplay::new(&[(640, 480)]);
    let err = display
        .request_host_mode(0, 0, 480)
        .expect_err("zero width");
    assert!(matches!(err, HostModeError::Dimensions { .. }));
}

#[test]
fn pack_dirty_emits_only_the_damaged_rows() {
    let display = GpuDisplay::new(&[(4, 2)]);
    display.with_frame(0, |frame| {
        frame.width = 4;
        frame.height = 2;
        frame.pixels = (0..32).collect();
        frame.dirty = Some(crate::protocol::Rect {
            x: 1,
            y: 0,
            width: 2,
            height: 1,
        });
        let (rect, bytes, partial) = frame.pack_dirty();
        assert!(partial);
        assert_eq!(rect.width, 2);
        assert_eq!(bytes, vec![4, 5, 6, 7, 8, 9, 10, 11]);
    });
}
