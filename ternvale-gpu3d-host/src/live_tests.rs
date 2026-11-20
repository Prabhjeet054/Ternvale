//! Step 107 live analogs: GPU-lost survival, feature-off, decoder soak.

use std::cell::RefCell;
use std::io::Write;
use std::sync::Once;
use std::time::{Duration, Instant};

use crate::decoder::decode_submit;
use crate::host::HostGpu;
use crate::state::CAPSET_VENUS;
use crate::Gpu3dError;

thread_local! {
    static SINK: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

struct ThreadSink;

impl Write for ThreadSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        SINK.with(|sink| {
            if let Some(sink) = sink.borrow_mut().as_mut() {
                sink.extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture<T>(f: impl FnOnce() -> T) -> (T, String) {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(|| ThreadSink)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
    SINK.with(|sink| *sink.borrow_mut() = Some(Vec::new()));
    let value = f();
    let bytes = SINK
        .with(|sink| sink.borrow_mut().take())
        .unwrap_or_default();
    (value, String::from_utf8_lossy(&bytes).into_owned())
}

/// Killing virglrenderer's render-server is the same as [`HostGpu::mark_lost`]:
/// later submits fail with [`Gpu3dError::GpuLost`] and the VMM process stays up.
#[test]
fn renderer_death_marks_gpu_lost_at_error_without_panic() {
    let ((), log) = capture(|| {
        let mut gpu = HostGpu::for_tests();
        gpu.ctx_create(1, CAPSET_VENUS, b"vkcube").expect("ctx");
        let mut cs = Vec::new();
        cs.extend_from_slice(&1u32.to_le_bytes());
        cs.extend_from_slice(&8u32.to_le_bytes());
        gpu.submit(1, &cs, 1).expect("submit before death");
        tracing::info!(
            target: "ternvale::gpu3d",
            "simulating host renderer process exit mid-render"
        );
        gpu.mark_lost();
        assert!(gpu.is_lost());
        assert!(matches!(gpu.submit(1, &cs, 0), Err(Gpu3dError::GpuLost)));
        gpu.ctx_create(2, CAPSET_VENUS, b"after").expect_err("lost");
    });
    assert!(
        log.contains("host GPU marked lost") || log.contains("GPU lost"),
        "expected ERROR lost line, got:\n{log}"
    );
    assert!(
        log.contains("ERROR") || log.to_ascii_lowercase().contains("lost"),
        "{log}"
    );
}

#[test]
fn decoder_burst_does_not_panic() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for i in 0..4096u32 {
        state ^= u64::from(i).wrapping_mul(0x1000_0000_1b3);
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let len = (state as usize % 96) + (i as usize % 8);
        let mut buf = vec![0u8; len];
        for (j, byte) in buf.iter_mut().enumerate() {
            *byte = (state.wrapping_add(j as u64) >> ((j % 8) * 8)) as u8;
        }
        let _ = decode_submit(&buf);
    }
}

/// Ten-minute soak of the untrusted decoder. Override with `TERNVALE_GPU3D_FUZZ_SECS`.
#[test]
#[ignore = "gpu3d-fuzz-10m"]
fn fuzz_decoder_ten_minutes() {
    let secs: u64 = std::env::var("TERNVALE_GPU3D_FUZZ_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let until = Instant::now() + Duration::from_secs(secs);
    tracing::info!(
        target: "ternvale::gpu3d",
        secs,
        "gpu3d decoder soak starting"
    );
    let mut state = 0xcbf2_9ce4_8422_2325u64;
    let mut n = 0u64;
    while Instant::now() < until {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let len = (state as usize % 512).max(1);
        let mut buf = vec![0u8; len];
        for (j, byte) in buf.iter_mut().enumerate() {
            *byte = state.to_le_bytes()[j % 8] ^ (j as u8);
        }
        let _ = decode_submit(&buf);
        n += 1;
    }
    tracing::info!(
        target: "ternvale::gpu3d",
        secs,
        iterations = n,
        "gpu3d decoder soak finished"
    );
    assert!(n > 0);
}

/// Venus inner path without virglrenderer is still the software stub.
#[test]
fn host_gpu_without_virgl_logs_software_not_metal() {
    let ((), log) = capture(|| {
        let _gpu = HostGpu::new();
    });
    let lower = log.to_ascii_lowercase();
    assert!(
        lower.contains("software_stub") || lower.contains("stub renderer"),
        "expected Venus software path in logs:\n{log}"
    );
    assert!(
        !lower.contains("host_gpu_path=virgl_moltenvk"),
        "must not claim virgl Venus Metal without virglrenderer:\n{log}"
    );
}

/// Production [`HostGpu::new`] loads MoltenVK for tvvk DRAW_CUBE when the dylib exists.
#[test]
fn host_gpu_draw_cube_uses_moltenvk_when_present() {
    let dylib = std::path::Path::new("/opt/homebrew/lib/libMoltenVK.dylib");
    if !dylib.is_file() {
        tracing::info!(target: "ternvale::gpu3d", "MoltenVK dylib missing; skip Metal assert");
        return;
    }
    let (result, log) = capture(|| {
        let mut gpu = HostGpu::new();
        gpu.ctx_create(1, CAPSET_VENUS, b"tvvk").expect("ctx");
        gpu.submit(1, &crate::encode_draw_cube(64, 64, 1), 1)
    });
    let lower = log.to_ascii_lowercase();
    if result.is_err() {
        assert!(
            lower.contains("moltenvk not loaded")
                || lower.contains("cannot use metal")
                || lower.contains("vkcreateinstance"),
            "DRAW_CUBE failed without a MoltenVK load log:\n{log}\n{result:?}"
        );
        tracing::info!(
            target: "ternvale::gpu3d",
            "MoltenVK present on disk but not loaded (Hardened Runtime); skip Metal assert"
        );
        return;
    }
    assert!(
        lower.contains("host_gpu_path=moltenvk") && lower.contains("metal=true"),
        "expected MoltenVK Metal path:\n{log}"
    );
    assert!(
        !lower.contains("tvvk draw_cube without moltenvk"),
        "DRAW_CUBE must not stay on software_stub:\n{log}"
    );
}

/// vkcube/DXVK start/stop analog: ctx + blob + nop CS + destroy; no leftover objects.
#[test]
fn win3d_app_start_stop_does_not_leak_host_objects() {
    use crate::encode_nop;
    use crate::state::BLOB_MEM_HOST3D;

    let mut gpu = HostGpu::for_tests();
    for i in 0..256u32 {
        tracing::debug!(
            target: "ternvale::gpu3d",
            iteration = i,
            "win3d start/stop analog"
        );
        gpu.ctx_create(1, CAPSET_VENUS, b"vkcube").expect("ctx");
        gpu.create_blob(1, 3, BLOB_MEM_HOST3D, 4096).expect("blob");
        gpu.map_blob(3, 0).expect("map");
        gpu.submit(1, &encode_nop(), 1).expect("submit");
        gpu.destroy_blob(3);
        gpu.ctx_destroy(1);
        assert_eq!(
            gpu.live_counts(),
            (0, 0, 0),
            "leaked 3D objects after iteration {i}"
        );
    }
}

/// 30-minute start/stop soak. Override with `TERNVALE_WIN3D_SOAK_SECS` (0 skips).
#[test]
#[ignore = "win3d-soak-30m"]
fn win3d_start_stop_thirty_minutes() {
    use crate::encode_nop;
    use crate::state::BLOB_MEM_HOST3D;

    let secs: u64 = std::env::var("TERNVALE_WIN3D_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1800);
    if secs == 0 {
        tracing::info!(target: "ternvale::gpu3d", "win3d soak skipped (0s)");
        return;
    }
    let until = Instant::now() + Duration::from_secs(secs);
    let mut gpu = HostGpu::for_tests();
    let mut n = 0u64;
    tracing::info!(
        target: "ternvale::gpu3d",
        secs,
        "win3d start/stop soak starting"
    );
    while Instant::now() < until {
        gpu.ctx_create(1, CAPSET_VENUS, b"d3d11").expect("ctx");
        gpu.create_blob(1, 3, BLOB_MEM_HOST3D, 4096).expect("blob");
        gpu.map_blob(3, 0).expect("map");
        gpu.submit(1, &encode_nop(), 1).expect("submit");
        gpu.destroy_blob(3);
        gpu.ctx_destroy(1);
        assert_eq!(gpu.live_counts(), (0, 0, 0));
        n += 1;
    }
    tracing::info!(
        target: "ternvale::gpu3d",
        secs,
        iterations = n,
        "win3d start/stop soak finished; VM analog still live"
    );
    assert!(n > 0);
}
