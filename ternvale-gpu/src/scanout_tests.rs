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
fn request_host_mode_rejects_bad_geometry() {
    let display = GpuDisplay::new(&[(640, 480)]);
    let err = display
        .request_host_mode(0, 0, 480)
        .expect_err("zero width");
    assert!(matches!(err, HostModeError::Dimensions { .. }));
}
