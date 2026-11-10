//! Shared checks every [`crate::VmBackend`] must pass.

use std::path::Path;

use ternvale_vmm::VmState;

use crate::{BackendError, StopMode, VmBackend};

/// Created → start → pause → resume → graceful stop → Stopped.
#[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
pub async fn lifecycle<B: VmBackend>(backend: &B) {
    assert_eq!(backend.state(), VmState::Created, "start in Created");
    backend.start().await.expect("start");
    assert_eq!(backend.state(), VmState::Running);
    backend.pause().await.expect("pause");
    assert_eq!(backend.state(), VmState::Paused);
    backend.resume().await.expect("resume");
    assert_eq!(backend.state(), VmState::Running);
    backend.stop(StopMode::Graceful).await.expect("stop");
    assert_eq!(backend.state(), VmState::Stopped);
}

/// Pause / resume / start rejected in the wrong state.
#[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
pub async fn invalid_transitions<B: VmBackend>(backend: &B) {
    assert_eq!(backend.state(), VmState::Created);
    let pause = backend.pause().await.expect_err("pause while created");
    assert!(
        matches!(pause, BackendError::InvalidTransition { op: "pause", .. }),
        "{pause}"
    );
    let resume = backend.resume().await.expect_err("resume while created");
    assert!(
        matches!(resume, BackendError::InvalidTransition { op: "resume", .. }),
        "{resume}"
    );
}

/// `query_stats` reports the configured CPU count on both the summary and the per-CPU list.
#[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
pub async fn stats_shape<B: VmBackend>(backend: &B, expect_cpus: u32) {
    backend.start().await.expect("start");
    let stats = backend.query_stats().await.expect("stats");
    assert_eq!(stats.cpus, expect_cpus, "status.cpus");
    assert_eq!(
        stats.cpu_entries, expect_cpus as usize,
        "per-cpu stats length"
    );
    assert_eq!(stats.state, VmState::Running);
    backend.stop(StopMode::Graceful).await.expect("stop");
}

/// Graceful stop from Running ends in Stopped.
#[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
pub async fn graceful_stop<B: VmBackend>(backend: &B) {
    backend.start().await.expect("start");
    backend.stop(StopMode::Graceful).await.expect("graceful");
    assert_eq!(backend.state(), VmState::Stopped);
}

/// Features the capabilities bitfield does not grant return [`BackendError::Unsupported`].
#[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
pub async fn unsupported_features<B: VmBackend>(backend: &B) {
    let caps = backend.capabilities();
    if !caps.custom_display_pipeline {
        let error = backend
            .display_surface()
            .await
            .expect_err("display without pipeline");
        assert!(
            matches!(
                error,
                BackendError::Unsupported {
                    feature: "custom_display_pipeline"
                }
            ),
            "{error}"
        );
    }
    if !caps.snapshots_disk && !caps.snapshots_memory {
        let error = backend
            .snapshot_save("x", Path::new("/tmp/x.snap"))
            .await
            .expect_err("snapshot");
        assert!(matches!(error, BackendError::Unsupported { .. }), "{error}");
    } else if !caps.nested_virt {
        // Native advertises snapshots but has no snapshot host in unit tests.
        let error = backend
            .snapshot_save("x", Path::new("/tmp/x.snap"))
            .await
            .expect_err("snapshot host");
        assert!(
            matches!(
                error,
                BackendError::Unsupported {
                    feature: "snapshots"
                }
            ),
            "{error}"
        );
    }
}

/// Run every conformance case. `make` must produce a fresh Created backend.
pub async fn run_all<F, Fut, B>(mut make: F, expect_cpus: u32)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = B>,
    B: VmBackend,
{
    lifecycle(&make().await).await;
    invalid_transitions(&make().await).await;
    stats_shape(&make().await, expect_cpus).await;
    graceful_stop(&make().await).await;
    unsupported_features(&make().await).await;
}
