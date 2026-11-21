//! Live privacy flags for `status.privacy` (config + capturing, no media).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use ternvale_config::PrivacySnapshot;

/// JSON for the optional `status.privacy` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyJson {
    pub microphone: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub microphone_reason: Option<String>,
    pub camera: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_reason: Option<String>,
    pub camera_transport: String,
    pub usb_controller: bool,
    pub usb_passthrough: Vec<UsbJson>,
    pub shares: Vec<ShareJson>,
    pub clipboard: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub printing: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub printers: Vec<String>,
}

/// USB passthrough row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbJson {
    pub port: u8,
    pub vendor_id: u16,
    pub product_id: u16,
}

/// Shared folder row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareJson {
    pub tag: String,
    pub host_path: String,
    pub read_only: bool,
}

/// Atomically updated capture flags.
pub struct PrivacyLive {
    snapshot: PrivacySnapshot,
    mic_capturing: Arc<AtomicBool>,
    camera_streaming: Arc<AtomicBool>,
    mic_denied: std::sync::Mutex<Option<String>>,
    camera_denied: std::sync::Mutex<Option<String>>,
}

impl PrivacyLive {
    /// From the VM config; flags start idle.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn from_snapshot(snapshot: PrivacySnapshot) -> Arc<Self> {
        Arc::new(Self {
            snapshot,
            mic_capturing: Arc::new(AtomicBool::new(false)),
            camera_streaming: Arc::new(AtomicBool::new(false)),
            mic_denied: std::sync::Mutex::new(None),
            camera_denied: std::sync::Mutex::new(None),
        })
    }

    /// Host microphone RX is running.
    pub fn mic_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.mic_capturing)
    }

    /// Agent camera frames are flowing.
    pub fn camera_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.camera_streaming)
    }

    /// Record a TCC / device denial (reason only).
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn deny_mic(&self, reason: String) {
        if let Ok(mut slot) = self.mic_denied.lock() {
            *slot = Some(reason);
        }
    }

    /// Record a camera denial.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn deny_camera(&self, reason: String) {
        if let Ok(mut slot) = self.camera_denied.lock() {
            *slot = Some(reason);
        }
    }

    /// Wire object (no samples).
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn json(&self) -> PrivacyJson {
        let mic_denied = self.mic_denied.lock().ok().and_then(|g| g.clone());
        let cam_denied = self.camera_denied.lock().ok().and_then(|g| g.clone());
        let microphone = if mic_denied.is_some() {
            "denied"
        } else if !self.snapshot.microphone {
            "off"
        } else if self.mic_capturing.load(Ordering::Relaxed) {
            "capturing"
        } else {
            "idle"
        };
        let camera = if cam_denied.is_some() {
            "denied"
        } else if !self.snapshot.camera {
            "off"
        } else if self.camera_streaming.load(Ordering::Relaxed) {
            "streaming"
        } else {
            "idle"
        };
        PrivacyJson {
            microphone: microphone.into(),
            microphone_reason: mic_denied,
            camera: camera.into(),
            camera_reason: cam_denied,
            camera_transport: self.snapshot.camera_transport.as_str().into(),
            usb_controller: self.snapshot.usb_controller,
            usb_passthrough: self
                .snapshot
                .usb_passthrough
                .iter()
                .map(|u| UsbJson {
                    port: u.port,
                    vendor_id: u.vendor_id,
                    product_id: u.product_id,
                })
                .collect(),
            shares: self
                .snapshot
                .shares
                .iter()
                .map(|s| ShareJson {
                    tag: s.tag.clone(),
                    host_path: s.host_path.clone(),
                    read_only: s.read_only,
                })
                .collect(),
            clipboard: if self.snapshot.clipboard {
                "agent".into()
            } else {
                "off".into()
            },
            printing: self.snapshot.printing,
            printers: self.snapshot.printers.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternvale_config::VmConfig;

    #[test]
    fn off_by_default() {
        let config = VmConfig::parse(
            "name = \"p\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n",
        )
        .expect("parse");
        let live = PrivacyLive::from_snapshot(config.privacy_snapshot());
        let json = live.json();
        assert_eq!(json.microphone, "off");
        assert_eq!(json.camera, "off");
        assert_eq!(json.clipboard, "off");
    }

    #[test]
    fn denied_and_capturing_states() {
        let config = VmConfig::parse(
            "name = \"p\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n[audio]\ninput = true\n[camera]\nenabled = true\n",
        )
        .expect("parse");
        let live = PrivacyLive::from_snapshot(config.privacy_snapshot());
        assert_eq!(live.json().microphone, "idle");
        assert_eq!(live.json().camera, "idle");
        live.mic_flag().store(true, Ordering::Relaxed);
        live.camera_flag().store(true, Ordering::Relaxed);
        assert_eq!(live.json().microphone, "capturing");
        assert_eq!(live.json().camera, "streaming");
        live.deny_mic("TERNVALE_MIC_DENIED is set".into());
        live.deny_camera("TERNVALE_CAMERA_DENIED is set".into());
        let json = live.json();
        assert_eq!(json.microphone, "denied");
        assert_eq!(json.camera, "denied");
        assert!(json
            .microphone_reason
            .as_ref()
            .is_some_and(|r| r.contains("MIC")));
        assert_eq!(json.camera_transport, "agent");
    }
}

#[cfg(test)]
#[path = "privacy_quality.rs"]
mod quality;
