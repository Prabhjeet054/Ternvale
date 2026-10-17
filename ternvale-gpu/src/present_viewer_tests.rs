//! The real Swift viewer (`tools/ternvale-viewer`, `--headless`) against a
//! [`Presenter`]: proves the two ends agree on the wire format.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::tests::{display, socket_path, spawn_lock, update};
use super::*;

/// Build the viewer with scripts/build-viewer.sh; `None` (test skipped) without swiftc.
fn built_viewer() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let have_swiftc = Command::new("/bin/sh")
        .args(["-c", "command -v swiftc"])
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !have_swiftc {
        eprintln!("skipping: swiftc not installed, cannot build tools/ternvale-viewer");
        return None;
    }
    let status = Command::new(root.join("scripts/build-viewer.sh"))
        .status()
        .expect("run build-viewer.sh");
    assert!(status.success(), "build-viewer.sh failed");
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    Some(target.join("viewer/ternvale-viewer"))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn swift_viewer_receives_frames_and_acks_them() {
    let Some(viewer) = built_viewer() else {
        return;
    };
    let display = display();
    update(&display, 0x11, 1);
    let path = socket_path();
    let presenter = Presenter::start(
        display.clone(),
        &path,
        PresentConfig {
            fps: 120,
            ..PresentConfig::default()
        },
    )
    .expect("start presenter");
    let mut child = {
        let _spawn = spawn_lock();
        Command::new(&viewer)
            .arg("--socket")
            .arg(&path)
            .args(["--headless", "--frames", "2"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn viewer")
    };
    let stats = presenter.stats();
    wait_for("first ack", || stats.presented.load(Ordering::Relaxed) == 1);
    update(&display, 0x22, 1);
    wait_for("viewer exit", || matches!(child.try_wait(), Ok(Some(_))));
    let output = child.wait_with_output().expect("viewer output");
    assert!(output.status.success(), "{:?}", output.status);
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let lines: Vec<_> = stdout.lines().collect();
    let bytes = (4 * 2 * 4) as usize;
    assert_eq!(
        lines,
        [
            format!(
                "frame sequence=1 width=4 height=2 format=2 bytes={bytes} fnv1a={:016x}",
                fnv1a(&vec![0x11; bytes])
            ),
            format!(
                "frame sequence=2 width=4 height=2 format=2 bytes={bytes} fnv1a={:016x}",
                fnv1a(&vec![0x22; bytes])
            ),
        ]
    );
    wait_for("second ack", || {
        stats.presented.load(Ordering::Relaxed) == 2
    });
    assert_eq!(stats.latency_samples.load(Ordering::Relaxed), 2);
    drop(presenter);
}
