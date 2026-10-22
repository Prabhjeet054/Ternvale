use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use super::*;

#[test]
fn request_host_mode_updates_modes_and_notifies() {
    let display = GpuDisplay::new(&[(1280, 800)]);
    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&calls);
    display.install_host_resize_hook(Box::new(move |scanout, w, h| {
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
fn request_host_mode_rejects_bad_geometry() {
    let display = GpuDisplay::new(&[(640, 480)]);
    let err = display
        .request_host_mode(0, 0, 480)
        .expect_err("zero width");
    assert!(matches!(err, HostModeError::Dimensions { .. }));
}
