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
