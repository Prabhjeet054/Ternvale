//! `[camera]`: host webcam into the guest. Default off (TCC + per-VM opt-in).

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::vm::VmConfig;

/// How the guest sees the host camera.
///
/// USB Video Class over xHCI needs **isochronous** transfers. The current
/// xHCI model only completes control and bulk rings (`ternvale-usb`). That
/// work is larger than this step (endpoint state, service interval, underrun
/// events). Until ISOCH exists, the host uses the **guest agent** path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CameraTransport {
    /// Do not present a camera.
    #[default]
    None,
    /// Host AVFoundation frames → agent (`camera_frame`). Guest helper /
    /// v4l2loopback / a Windows UMDF sidecar consumes them.
    Agent,
}

impl CameraTransport {
    /// Wire / log name.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Agent => "agent",
        }
    }
}

/// The `[camera]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraSection {
    /// Host camera into the guest. Default `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enabled: bool,
}

impl CameraSection {
    /// No extra validation today.
    pub fn validate(&self) -> Result<(), ConfigError> {
        Ok(())
    }

    /// Transport used when `enabled` (never silent UVC).
    #[must_use]
    pub fn transport(&self) -> CameraTransport {
        if self.enabled {
            CameraTransport::Agent
        } else {
            CameraTransport::None
        }
    }
}

impl VmConfig {
    /// `[camera]` or defaults (off).
    pub fn camera_section(&self) -> CameraSection {
        self.camera.clone().unwrap_or_default()
    }

    /// Host webcam. Off unless `[camera] enabled = true`.
    pub fn camera_enabled(&self) -> bool {
        self.camera_section().enabled
    }
}

#[cfg(test)]
mod tests {
    use super::{CameraSection, CameraTransport};

    #[test]
    fn camera_defaults_off() {
        let camera = CameraSection::default();
        assert!(!camera.enabled);
        assert_eq!(camera.transport(), CameraTransport::None);
    }

    #[test]
    fn enabled_uses_agent_not_uvc() {
        let camera = CameraSection { enabled: true };
        assert_eq!(camera.transport(), CameraTransport::Agent);
    }
}
