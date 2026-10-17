use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use super::*;
use crate::present_protocol::{FrameHeader, ViewerMessage, FRAME_HEADER_LEN, VERSION};
use crate::present_session;
use crate::resource::PixelFormat;
use crate::test_log;

const W: u32 = 4;
const H: u32 = 2;

/// Held while sockets are open and while the viewer process is spawned, so the
/// child never inherits a test's socket and keeps it from reporting EOF.
static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) fn spawn_lock() -> std::sync::MutexGuard<'static, ()> {
    SPAWN.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn display() -> GpuDisplay {
    GpuDisplay::new(&[(W, H)])
}

/// Make the scanout show `tag`-filled pixels and advance its sequence by `step`.
pub(super) fn update(display: &GpuDisplay, tag: u8, step: u64) {
    display.with_frame(0, |frame| {
        frame.width = W;
        frame.height = H;
        frame.format = Some(PixelFormat::B8g8r8x8);
        frame.pixels = vec![tag; (W * H * 4) as usize];
        frame.sequence += step;
        frame.updated = Some(Instant::now());
    });
}

pub(super) fn socket_path() -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    std::env::temp_dir().join(format!(
        "tv-present-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

struct FakeViewer(UnixStream);

impl FakeViewer {
    fn hello(stream: UnixStream) -> Self {
        let mut viewer = Self(stream);
        viewer.send(ViewerMessage::Hello { version: VERSION });
        viewer
    }

    fn send(&mut self, message: ViewerMessage) {
        self.0
            .write_all(&message.encode())
            .expect("send to presenter");
    }

    fn frame(&mut self) -> (FrameHeader, Vec<u8>) {
        self.0
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        let mut raw = [0u8; FRAME_HEADER_LEN];
        self.0.read_exact(&mut raw).expect("frame header");
        let header = FrameHeader::decode(&raw).expect("valid header");
        let mut pixels = vec![0u8; header.payload_len as usize];
        self.0.read_exact(&mut pixels).expect("frame payload");
        (header, pixels)
    }

    fn ack(&mut self, header: &FrameHeader) {
        self.send(ViewerMessage::Presented {
            sequence: header.sequence,
        });
    }
}

/// Run a session on this thread (so its logs are captured) against `viewer`.
fn session(
    fps: u32,
    display: &GpuDisplay,
    viewer: impl FnOnce(UnixStream) + Send + 'static,
) -> (Result<&'static str, String>, String, PresentStats) {
    let config = PresentConfig {
        fps,
        ..PresentConfig::default()
    };
    session_with(config, display, viewer)
}

fn session_with(
    config: PresentConfig,
    display: &GpuDisplay,
    viewer: impl FnOnce(UnixStream) + Send + 'static,
) -> (Result<&'static str, String>, String, PresentStats) {
    let _spawn = spawn_lock();
    let (ours, theirs) = UnixStream::pair().expect("socket pair");
    let peer = std::thread::spawn(move || viewer(theirs));
    let stats = PresentStats::default();
    let stop = AtomicBool::new(false);
    let (result, logs) = test_log::capture(|| {
        present_session::serve(ours, display, config, &stats, &stop).map_err(|e| e.to_string())
    });
    peer.join().expect("fake viewer");
    (result, logs, stats)
}

#[test]
fn session_streams_frames_and_logs_present_latency() {
    let display = display();
    update(&display, 0x11, 1);
    let shared = display.clone();
    let (result, logs, stats) = session(240, &display, move |stream| {
        let mut viewer = FakeViewer::hello(stream);
        let (header, pixels) = viewer.frame();
        assert_eq!((header.width, header.height, header.stride), (W, H, W * 4));
        assert_eq!((header.format, header.sequence), (2, 1));
        assert!(pixels.iter().all(|&b| b == 0x11));
        viewer.ack(&header);
        update(&shared, 0x22, 1);
        let (header, pixels) = viewer.frame();
        assert_eq!(header.sequence, 2);
        assert!(pixels.iter().all(|&b| b == 0x22));
        viewer.ack(&header);
        std::thread::sleep(Duration::from_millis(30));
    });
    assert_eq!(result, Ok("viewer closed the connection"));
    assert_eq!(stats.viewers.load(Ordering::Relaxed), 1);
    assert_eq!(stats.sent.load(Ordering::Relaxed), 2);
    assert_eq!(stats.presented.load(Ordering::Relaxed), 2);
    assert_eq!(stats.dropped.load(Ordering::Relaxed), 0);
    assert_eq!(stats.latency_samples.load(Ordering::Relaxed), 2);
    assert!(stats.average_latency_us().is_some());
    assert!(logs.contains("display viewer hello accepted"), "{logs}");
    assert!(logs.contains("frame sent"), "{logs}");
    let presented: Vec<_> = logs
        .lines()
        .filter(|l| l.contains("frame presented"))
        .collect();
    assert_eq!(presented.len(), 2, "{logs}");
    assert!(presented[0].contains("DEBUG"), "{logs}");
    assert!(presented[0].contains("sequence=1"), "{logs}");
    assert!(presented[0].contains("flush_to_present_us="), "{logs}");
    assert!(presented[0].contains("send_to_present_us="), "{logs}");
}

#[test]
fn session_logs_latency_windows() {
    let display = display();
    update(&display, 1, 1);
    let shared = display.clone();
    let config = PresentConfig {
        fps: 240,
        report_every: Duration::from_millis(40),
        ..PresentConfig::default()
    };
    let (result, logs, _) = session_with(config, &display, move |stream| {
        let mut viewer = FakeViewer::hello(stream);
        for tag in 2..=4u8 {
            let (header, _) = viewer.frame();
            viewer.ack(&header);
            std::thread::sleep(Duration::from_millis(30));
            update(&shared, tag, 2);
        }
        let (header, _) = viewer.frame();
        viewer.ack(&header);
        std::thread::sleep(Duration::from_millis(60));
    });
    assert_eq!(result, Ok("viewer closed the connection"));
    let windows: Vec<_> = logs
        .lines()
        .filter(|l| l.contains("display present window"))
        .collect();
    assert!(windows.len() >= 2, "{logs}");
    assert!(windows.iter().all(|l| l.contains("DEBUG")), "{logs}");
    assert!(windows[0].contains("why=\"interval\""), "{logs}");
    assert!(
        windows
            .last()
            .is_some_and(|l| l.contains("why=\"session end\"")),
        "{logs}"
    );
    let field = |name: &str| -> u64 {
        windows
            .iter()
            .map(|l| {
                l.split_whitespace()
                    .find_map(|kv| kv.strip_prefix(name))
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0)
            })
            .sum()
    };
    assert_eq!(
        field("presented="),
        4,
        "every ack lands in one window: {logs}"
    );
    assert_eq!(
        field("dropped="),
        3,
        "each update skipped one sequence: {logs}"
    );
    assert!(logs.contains("avg_flush_to_present_us="), "{logs}");
}

#[test]
fn latency_window_averages_and_resets() {
    let start = Instant::now();
    let mut window = crate::present_window::LatencyWindow::new(start);
    assert_eq!(window.average_us(), None);
    window.presented(Some(100));
    window.presented(Some(300));
    window.presented(None);
    window.dropped(2);
    assert_eq!(window.average_us(), Some(200));
    assert!(!window.report_if_due(start + Duration::from_secs(1), Duration::from_secs(10)));
    let (due, logs) = test_log::capture(|| {
        window.report_if_due(start + Duration::from_secs(10), Duration::from_secs(10))
    });
    assert!(due);
    assert!(logs.contains("window_ms=10000"), "{logs}");
    assert!(logs.contains("presented=3"), "{logs}");
    assert!(logs.contains("fps=0.3"), "{logs}");
    assert!(logs.contains("dropped=2"), "{logs}");
    assert!(logs.contains("avg_flush_to_present_us=200"), "{logs}");
    assert!(logs.contains("max_flush_to_present_us=300"), "{logs}");
    assert_eq!(window.average_us(), None, "a new window starts empty");
}

#[test]
fn busy_viewer_holds_frames_and_logs_drops() {
    let display = display();
    update(&display, 1, 1);
    let shared = display.clone();
    let (result, logs, stats) = session(240, &display, move |stream| {
        let mut viewer = FakeViewer::hello(stream);
        let (first, _) = viewer.frame();
        update(&shared, 4, 3);
        std::thread::sleep(Duration::from_millis(50));
        viewer.ack(&first);
        let (next, pixels) = viewer.frame();
        assert_eq!(next.sequence, 4, "only the newest frame is sent");
        assert!(pixels.iter().all(|&b| b == 4));
        viewer.ack(&next);
        std::thread::sleep(Duration::from_millis(30));
    });
    assert_eq!(result, Ok("viewer closed the connection"));
    assert_eq!(stats.dropped.load(Ordering::Relaxed), 2);
    let dropped = logs
        .lines()
        .find(|l| l.contains("frames dropped"))
        .unwrap_or_else(|| panic!("no drop line: {logs}"));
    assert!(dropped.contains("DEBUG"), "{dropped}");
    assert!(dropped.contains("count=2"), "{dropped}");
    assert!(dropped.contains("reason=\"viewer busy\""), "{dropped}");
}

#[test]
fn fps_cap_skips_intermediate_frames() {
    let display = display();
    update(&display, 1, 1);
    let shared = display.clone();
    let (result, logs, stats) = session(10, &display, move |stream| {
        let mut viewer = FakeViewer::hello(stream);
        let (first, _) = viewer.frame();
        viewer.ack(&first);
        std::thread::sleep(Duration::from_millis(20));
        let before = Instant::now();
        update(&shared, 3, 2);
        let (next, _) = viewer.frame();
        assert_eq!(next.sequence, 3);
        assert!(
            before.elapsed() <= Duration::from_millis(150),
            "next tick within 1/fps"
        );
        viewer.ack(&next);
        std::thread::sleep(Duration::from_millis(30));
    });
    assert_eq!(result, Ok("viewer closed the connection"));
    assert_eq!(stats.dropped.load(Ordering::Relaxed), 1);
    assert!(logs.contains("reason=\"fps cap\""), "{logs}");
}

#[test]
fn unsolicited_ack_is_ignored_with_warning() {
    let display = display();
    update(&display, 1, 1);
    let (result, logs, stats) = session(240, &display, move |stream| {
        let mut viewer = FakeViewer::hello(stream);
        let (first, _) = viewer.frame();
        viewer.send(ViewerMessage::Presented { sequence: 99 });
        viewer.ack(&first);
        std::thread::sleep(Duration::from_millis(30));
    });
    assert_eq!(result, Ok("viewer closed the connection"));
    assert_eq!(stats.presented.load(Ordering::Relaxed), 1);
    assert!(logs.contains("WARN"), "{logs}");
    assert!(logs.contains("not in flight"), "{logs}");
}

#[test]
fn wrong_version_hello_is_refused() {
    let display = display();
    let (result, _, stats) = session(30, &display, |mut stream| {
        let mut hello = ViewerMessage::Hello { version: VERSION }.encode();
        hello[4] = 7;
        stream.write_all(&hello).expect("send hello");
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).expect("presenter closes");
        assert!(rest.is_empty(), "no frame for an incompatible viewer");
    });
    let err = result.expect_err("refused");
    assert!(err.contains("wire version 7"), "{err}");
    assert_eq!(stats.viewers.load(Ordering::Relaxed), 0);
}

#[test]
fn ack_before_hello_is_refused() {
    let display = display();
    let (result, _, _) = session(30, &display, |stream| {
        let mut viewer = FakeViewer(stream);
        viewer.send(ViewerMessage::Presented { sequence: 1 });
    });
    let err = result.expect_err("refused");
    assert!(err.contains("before hello"), "{err}");
}

#[test]
fn presenter_serves_socket_and_cleans_up() {
    let _spawn = spawn_lock();
    let display = display();
    update(&display, 9, 1);
    let path = socket_path();
    std::fs::write(&path, b"stale").expect("stale file");
    let presenter = Presenter::start(display.clone(), &path, PresentConfig::default())
        .expect("start presenter");
    assert_eq!(presenter.socket(), path.as_path());
    let mut viewer = FakeViewer::hello(UnixStream::connect(&path).expect("connect"));
    let (header, _) = viewer.frame();
    viewer.ack(&header);
    drop(viewer);
    let mut reconnect = FakeViewer::hello(UnixStream::connect(&path).expect("reconnect"));
    let (again, _) = reconnect.frame();
    assert_eq!(again.sequence, 1, "a new viewer gets the current frame");
    let stats = presenter.stats();
    drop(presenter);
    assert!(!path.exists(), "socket removed on stop");
    assert_eq!(stats.viewers.load(Ordering::Relaxed), 2);
    assert_eq!(stats.sent.load(Ordering::Relaxed), 2);
}

#[test]
fn presenter_rejects_bad_config() {
    let path = socket_path();
    for config in [
        PresentConfig {
            fps: 0,
            ..PresentConfig::default()
        },
        PresentConfig {
            fps: MAX_FPS + 1,
            ..PresentConfig::default()
        },
        PresentConfig {
            scanout: 1,
            ..PresentConfig::default()
        },
        PresentConfig {
            report_every: Duration::ZERO,
            ..PresentConfig::default()
        },
    ] {
        assert!(
            matches!(
                Presenter::start(display(), &path, config),
                Err(PresentError::Config(_))
            ),
            "{config:?}"
        );
    }
    assert!(!path.exists());
}
