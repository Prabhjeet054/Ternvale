//! Step 109 quality analogs: idle bytes, tearing-bar pacing, HiDPI wire,
//! independent scanout resize. The 60-second idle soak is `#[ignore]`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::tests::{display, session, session_with, socket_path, spawn_lock, update, FakeViewer};
use super::*;
use crate::present_protocol::{ViewerMessage, FRAME_FLAG_PARTIAL};
use crate::present_quality::PresentQuality;
use crate::protocol::Rect;
use crate::resource::PixelFormat;

const W: u32 = 4;
const H: u32 = 2;

fn paint_bar(display: &GpuDisplay, x: u32) {
    display.with_frame(0, |frame| {
        frame.width = W;
        frame.height = H;
        frame.format = Some(PixelFormat::B8g8r8x8);
        if frame.pixels.len() != (W * H * 4) as usize {
            frame.pixels = vec![0u8; (W * H * 4) as usize];
        }
        let bpp = 4usize;
        let col = (x % W) as usize;
        for row in 0..H as usize {
            let at = row * W as usize * bpp + col * bpp;
            frame.pixels[at..at + bpp].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        }
        frame.sequence += 1;
        frame.updated = Some(Instant::now());
        frame.dirty = Some(Rect {
            x: x % W,
            y: 0,
            width: 1,
            height: H,
        });
    });
}

/// Process CPU time (user+sys analog via the process clock).
fn process_cpu() -> Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid timespec; CLOCK_PROCESS_CPUTIME_ID is the
    // calling process's CPU clock on Darwin and Linux.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    assert_eq!(rc, 0, "clock_gettime CLOCK_PROCESS_CPUTIME_ID");
    Duration::new(ts.tv_sec.max(0) as u64, ts.tv_nsec.max(0) as u32)
}

fn write_report(name: &str, body: &str) {
    let dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target"));
    let path = dir.join(format!("present-quality-{name}.txt"));
    if let Err(error) = std::fs::write(&path, body) {
        tracing::warn!(
            target: "ternvale::gpu",
            path = %path.display(),
            error = %error,
            "could not write present-quality report"
        );
    }
}

fn sample_pcpu(pid: u32) -> f32 {
    let output = std::process::Command::new("ps")
        .args(["-o", "pcpu=", "-p", &pid.to_string()])
        .output();
    let Ok(output) = output else {
        return 0.0;
    };
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0.0)
}

#[test]
fn static_scanout_sends_no_bytes_after_the_first_frame() {
    let display = display();
    update(&display, 0xaa, 1);
    let (result, _, stats) = session(60, &display, |stream| {
        let mut viewer = FakeViewer::hello(stream);
        let (header, _) = viewer.frame();
        viewer.ack(&header);
        std::thread::sleep(Duration::from_millis(200));
    });
    assert_eq!(result, Ok("viewer closed the connection"));
    assert_eq!(stats.sent.load(Ordering::Relaxed), 1);
    assert_eq!(stats.dropped.load(Ordering::Relaxed), 0);
    assert_eq!(stats.upload_bytes.load(Ordering::Relaxed), 32);
}

#[test]
fn tearing_bar_paces_without_drops_and_uploads_one_column() {
    let display = display();
    update(&display, 0x11, 1);
    let shared = display.clone();
    let fps = 60u32;
    let frames = 30u32;
    let pacing = Arc::new((AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)));
    let pacing_viewer = Arc::clone(&pacing);
    let (result, _, stats) = session(fps, &display, move |stream| {
        let mut viewer = FakeViewer::hello(stream);
        let (first, _) = viewer.frame();
        viewer.ack(&first);
        let mut gaps = Vec::new();
        let mut last = Instant::now();
        for i in 0..frames {
            paint_bar(&shared, i);
            let (header, pixels) = viewer.frame();
            let now = Instant::now();
            gaps.push(now.saturating_duration_since(last));
            last = now;
            assert_eq!(header.flags & FRAME_FLAG_PARTIAL, FRAME_FLAG_PARTIAL);
            assert_eq!(header.payload_len, 8, "one column, two rows, BGRA");
            assert_eq!(pixels.len(), 8);
            viewer.ack(&header);
        }
        let micros = |d: Duration| u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        let mean_us: u64 = gaps.iter().copied().map(micros).sum::<u64>() / u64::from(frames);
        let max_us = gaps.iter().copied().map(micros).max().unwrap_or(0);
        let min_us = gaps.iter().copied().map(micros).min().unwrap_or(0);
        tracing::info!(
            target: "ternvale::gpu",
            frames,
            mean_us,
            max_us,
            min_us,
            fps,
            "tearing-bar pacing analog"
        );
        assert!(
            mean_us >= 8_000 && mean_us <= 40_000,
            "mean inter-frame {mean_us} us not near 1/{fps}s"
        );
        assert!(max_us < 80_000, "pacing stall {max_us} us");
        pacing_viewer.0.store(mean_us, Ordering::Relaxed);
        pacing_viewer.1.store(min_us, Ordering::Relaxed);
        pacing_viewer.2.store(max_us, Ordering::Relaxed);
    });
    assert_eq!(result, Ok("viewer closed the connection"));
    assert_eq!(stats.dropped.load(Ordering::Relaxed), 0);
    assert_eq!(stats.sent.load(Ordering::Relaxed), u64::from(1 + frames));
    let avg = stats.average_latency_us().expect("latency samples");
    let upload = stats.upload_bytes.load(Ordering::Relaxed);
    assert_eq!(upload, 32 + u64::from(frames) * 8);
    write_report(
        "tearing",
        &format!(
            "sent={} dropped=0 upload_bytes={upload} avg_flush_to_present_us={avg} max_flush_to_present_us={} pace_mean_us={} pace_min_us={} pace_max_us={}\n",
            stats.sent.load(Ordering::Relaxed),
            stats.latency_max_us.load(Ordering::Relaxed),
            pacing.0.load(Ordering::Relaxed),
            pacing.1.load(Ordering::Relaxed),
            pacing.2.load(Ordering::Relaxed),
        ),
    );
}

#[test]
fn hidpi_scale_travels_on_the_frame_header() {
    let display = display();
    display.set_quality(PresentQuality {
        scale_x100: 200,
        ..PresentQuality::default()
    });
    update(&display, 0x44, 1);
    let config = PresentConfig {
        fps: 60,
        quality: PresentQuality {
            scale_x100: 200,
            ..PresentQuality::default()
        },
        ..PresentConfig::default()
    };
    let (result, _, _) = session_with(config, &display, |stream| {
        let mut viewer = FakeViewer::hello(stream);
        let (header, _) = viewer.frame();
        assert_eq!(header.scale_x100, 200);
        assert_eq!(
            header.flags & crate::present_protocol::FRAME_FLAG_DISPLAY_P3,
            0
        );
        viewer.ack(&header);
        std::thread::sleep(Duration::from_millis(20));
    });
    assert_eq!(result, Ok("viewer closed the connection"));
}

#[test]
fn second_scanout_set_size_ex_does_not_resize_the_first() {
    let display = GpuDisplay::new(&[(W, H), (W, H)]);
    update(&display, 0x11, 1);
    display.with_frame(1, |frame| {
        frame.width = W;
        frame.height = H;
        frame.format = Some(PixelFormat::B8g8r8x8);
        frame.pixels = vec![0x22; (W * H * 4) as usize];
        frame.sequence = 1;
        frame.updated = Some(Instant::now());
        frame.dirty = Some(Rect {
            x: 0,
            y: 0,
            width: W,
            height: H,
        });
    });
    let config = PresentConfig {
        fps: 120,
        all_scanouts: true,
        ..PresentConfig::default()
    };
    let shared = display.clone();
    let (result, _, stats) = session_with(config, &display, move |stream| {
        let mut viewer = FakeViewer::hello(stream);
        let mut saw = [false, false];
        for _ in 0..2 {
            let (header, _) = viewer.frame();
            saw[header.scanout as usize] = true;
            viewer.ack(&header);
        }
        assert!(saw[0] && saw[1], "both scanouts presented");
        viewer.send(ViewerMessage::SetSizeEx {
            scanout: 1,
            width: 800,
            height: 600,
        });
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(shared.mode(0), Some((W, H)));
        assert_eq!(shared.mode(1), Some((800, 600)));
    });
    assert_eq!(result, Ok("viewer closed the connection"));
    assert!(stats.sent.load(Ordering::Relaxed) >= 2);
}

/// 60s static scanout with the real headless viewer. GPU is not sampled
/// (headless does not create a CAMetalLayer).
#[test]
#[ignore = "step109-idle-60s"]
fn static_desktop_idle_sixty_seconds() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .try_init();
    let Some(viewer) = super::viewer_tests::built_viewer() else {
        tracing::warn!(target: "ternvale::gpu", "swiftc missing; idle 60s skipped");
        return;
    };
    let display = display();
    update(&display, 0x11, 1);
    let path = socket_path();
    let metrics = Arc::new(ternvale_metrics::Metrics::new());
    let presenter = Presenter::start_with_input_metrics(
        display,
        &path,
        PresentConfig {
            fps: 30,
            ..PresentConfig::default()
        },
        None,
        Some(Arc::clone(&metrics)),
    )
    .expect("start presenter");
    let mut child = {
        let _spawn = spawn_lock();
        std::process::Command::new(&viewer)
            .arg("--socket")
            .arg(&path)
            .arg("--headless")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("spawn viewer")
    };
    let stats = presenter.stats();
    let deadline_first = Instant::now() + Duration::from_secs(10);
    while stats.presented.load(Ordering::Relaxed) == 0 {
        assert!(Instant::now() < deadline_first, "first frame not presented");
        std::thread::sleep(Duration::from_millis(10));
    }
    let sent_after_first = stats.sent.load(Ordering::Relaxed);
    let upload_after_first = stats.upload_bytes.load(Ordering::Relaxed);
    let cpu0 = process_cpu();
    let wall0 = Instant::now();
    let viewer_pid = child.id();
    let mut viewer_pcpu_max = 0.0f32;
    while wall0.elapsed() < Duration::from_secs(60) {
        viewer_pcpu_max = viewer_pcpu_max.max(sample_pcpu(viewer_pid));
        std::thread::sleep(Duration::from_millis(500));
    }
    let cpu = process_cpu().saturating_sub(cpu0);
    let sent = stats.sent.load(Ordering::Relaxed);
    let upload = stats.upload_bytes.load(Ordering::Relaxed);
    let presented = stats.presented.load(Ordering::Relaxed);
    let dropped = stats.dropped.load(Ordering::Relaxed);
    let snap = metrics.snapshot();
    let avg = stats.average_latency_us().unwrap_or(0);
    tracing::info!(
        target: "ternvale::gpu",
        sent,
        presented,
        dropped,
        upload_bytes = upload,
        sent_after_first,
        upload_after_first,
        cpu_ms = cpu.as_millis() as u64,
        viewer_pid,
        viewer_pcpu_max = %format!("{viewer_pcpu_max:.2}"),
        present_frames = snap.present.frames,
        present_upload = snap.present.upload_bytes,
        avg_flush_to_present_us = avg,
        "static desktop 60s idle analog"
    );
    write_report(
        "idle60",
        &format!(
            "wall_s=60 sent={sent} presented={presented} dropped={dropped} upload_bytes={upload} upload_after_first={upload_after_first} cpu_ms={} viewer_pcpu_max={viewer_pcpu_max:.2} avg_flush_to_present_us={avg} max_flush_to_present_us={} metrics_frames={} metrics_upload={}\n",
            cpu.as_millis(),
            stats.latency_max_us.load(Ordering::Relaxed),
            snap.present.frames,
            snap.present.upload_bytes,
        ),
    );
    assert_eq!(sent, sent_after_first, "idle must not send more frames");
    assert_eq!(upload, upload_after_first, "idle must not upload pixels");
    assert_eq!(dropped, 0);
    assert!(
        cpu < Duration::from_secs(3),
        "process CPU {cpu:?} over 60s is not near idle"
    );
    drop(presenter);
    let _ = child.wait();
}
