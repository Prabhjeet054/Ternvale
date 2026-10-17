//! The real Swift viewer (`tools/ternvale-viewer`, `--headless`) against a
//! [`Presenter`]: proves the two ends agree on the wire format.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::input_tests::Recorder;
use super::tests::{display, socket_path, spawn_lock, update};
use super::*;

/// Build the viewer with scripts/build-viewer.sh once per run; `None` (test
/// skipped) without swiftc.
fn built_viewer() -> Option<PathBuf> {
    static BUILT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    BUILT.get_or_init(build_viewer).clone()
}

fn build_viewer() -> Option<PathBuf> {
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

#[test]
fn swift_viewer_input_messages_decode() {
    let Some(viewer) = built_viewer() else {
        return;
    };
    let display = display();
    update(&display, 0x33, 1);
    let path = socket_path();
    let recorder = Arc::new(Recorder::default());
    let target: Arc<dyn InputTarget> = recorder.clone();
    let presenter =
        Presenter::start_with_input(display, &path, PresentConfig::default(), Some(target))
            .expect("start presenter");
    let mut child = {
        let _spawn = spawn_lock();
        Command::new(&viewer)
            .arg("--socket")
            .arg(&path)
            .args(["--headless", "--frames", "1", "--script-input"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn viewer")
    };
    let received = || recorder.0.lock().expect("recorder").len();
    wait_for("scripted input", || received() == 6);
    let status = child.wait().expect("viewer exit");
    assert!(status.success(), "{status:?}");
    let (key, button) = (
        |pressed| ViewerInput::Key {
            mac_keycode: 0,
            pressed,
        },
        |pressed| ViewerInput::Button {
            button: crate::PointerButton::Left,
            pressed,
        },
    );
    assert_eq!(
        *recorder.0.lock().expect("recorder"),
        [
            key(true),
            key(false),
            ViewerInput::Pointer { x: 16384, y: 8192 },
            button(true),
            button(false),
            ViewerInput::Scroll { dx: 0, dy: 1 },
        ]
    );
    drop(presenter);
}
