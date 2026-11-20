//! Probe host microphone / camera permission. Reasons are logged; samples are not.

use crate::HostAudioError;

/// Why capture was not started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDenied {
    /// `microphone` or `camera`.
    pub device: &'static str,
    /// Human-readable TCC / device reason (never sample data).
    pub reason: String,
}

impl std::fmt::Display for PermissionDenied {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} permission denied: {}",
            self.device, self.reason
        )
    }
}

/// Tests and CI: treat capture as denied without touching TCC.
pub const DENY_MIC_ENV: &str = "TERNVALE_MIC_DENIED";
/// Tests and CI: treat the camera as denied.
pub const DENY_CAMERA_ENV: &str = "TERNVALE_CAMERA_DENIED";
/// Tests: present a synthetic camera (not a live sensor).
pub const CAMERA_TEST_ENV: &str = "TERNVALE_CAMERA_TEST";

fn env_truthy(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v != "0" && v != "false")
}

/// Microphone: opt-in configs still fail closed when TCC/env denies.
#[tracing::instrument(level = "debug", target = "ternvale::virtio::sound", skip_all)]
pub fn probe_microphone() -> Result<(), PermissionDenied> {
    if env_truthy(DENY_MIC_ENV) {
        let denied = PermissionDenied {
            device: "microphone",
            reason: format!("{DENY_MIC_ENV} is set"),
        };
        tracing::warn!(
            target: "ternvale::virtio::sound",
            reason = %denied.reason,
            "host microphone denied; guest will see no capture stream"
        );
        return Err(denied);
    }
    let host = cpal::default_host();
    use cpal::traits::HostTrait;
    if host.default_input_device().is_none() {
        let denied = PermissionDenied {
            device: "microphone",
            reason: "no default input device (check System Settings → Microphone)".into(),
        };
        tracing::warn!(
            target: "ternvale::virtio::sound",
            reason = %denied.reason,
            "host microphone denied; guest will see no capture stream"
        );
        return Err(denied);
    }
    tracing::info!(
        target: "ternvale::virtio::sound",
        "host microphone probe ok (TCC prompt may still appear on first capture)"
    );
    Ok(())
}

/// Camera: live AVFoundation is requested by the Mac app; CLI tests use env.
#[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
pub fn probe_camera() -> Result<(), PermissionDenied> {
    if env_truthy(DENY_CAMERA_ENV) {
        let denied = PermissionDenied {
            device: "camera",
            reason: format!("{DENY_CAMERA_ENV} is set"),
        };
        tracing::warn!(
            target: "ternvale::gpu",
            reason = %denied.reason,
            "host camera denied; guest will see no webcam"
        );
        return Err(denied);
    }
    if env_truthy(CAMERA_TEST_ENV) {
        tracing::info!(
            target: "ternvale::gpu",
            "camera test pattern enabled (not a live sensor)"
        );
        return Ok(());
    }
    tracing::info!(
        target: "ternvale::gpu",
        "host camera probe: agent transport (no xHCI UVC); live frames from AVFoundation / test pattern"
    );
    Ok(())
}

/// Map a host-audio error to a permission denial (no PCM in the text).
#[tracing::instrument(level = "debug", target = "ternvale::virtio::sound", skip_all)]
pub fn deny_mic(error: &HostAudioError) -> PermissionDenied {
    PermissionDenied {
        device: "microphone",
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_denial_does_not_open_hardware() {
        std::env::set_var(DENY_MIC_ENV, "1");
        let err = probe_microphone().expect_err("denied");
        std::env::remove_var(DENY_MIC_ENV);
        assert_eq!(err.device, "microphone");
        assert!(err.reason.contains(DENY_MIC_ENV));
    }
}
