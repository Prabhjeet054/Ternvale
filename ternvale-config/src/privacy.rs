//! Snapshot of per-VM passthrough toggles (mic, camera, USB, shares, clipboard).

use crate::camera::CameraTransport;
use crate::vm::VmConfig;

/// One shared-folder row for the privacy tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharePrivacy {
    /// virtio-fs tag.
    pub tag: String,
    /// Host directory (display only).
    pub host_path: String,
    /// Guest writes fail when true.
    pub read_only: bool,
}

/// One USB passthrough row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbPrivacy {
    /// Guest root port.
    pub port: u8,
    /// `idVendor`.
    pub vendor_id: u16,
    /// `idProduct`.
    pub product_id: u16,
}

/// Configured privacy surface (not live TCC or capturing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivacySnapshot {
    /// `[audio] input`.
    pub microphone: bool,
    /// `[camera] enabled`.
    pub camera: bool,
    /// How a camera would be presented.
    pub camera_transport: CameraTransport,
    /// `[usb] enabled`.
    pub usb_controller: bool,
    /// Claimed host USB devices.
    pub usb_passthrough: Vec<UsbPrivacy>,
    /// `[[shares]]`.
    pub shares: Vec<SharePrivacy>,
    /// Guest agent clipboard (vsock agent).
    pub clipboard: bool,
    /// `[print] enabled`.
    pub printing: bool,
    /// Shared host printer names (plus PDF when configured).
    pub printers: Vec<String>,
}

impl VmConfig {
    /// Passthrough flags for the settings privacy tab and `status.privacy`.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name))]
    pub fn privacy_snapshot(&self) -> PrivacySnapshot {
        let usb = self.usb_section();
        let camera = self.camera_section();
        let clipboard = self.vsock.as_ref().is_some_and(|section| section.agent);
        let print = self.print.as_ref();
        let printing = print.is_some_and(|section| section.enabled);
        let mut printers = Vec::new();
        if printing {
            if print.is_some_and(|section| section.pdf) {
                printers.push("Print to PDF on Mac".into());
            }
            if let Some(section) = print {
                printers.extend(section.share.iter().cloned());
            }
        }
        let snapshot = PrivacySnapshot {
            microphone: self.audio_input_enabled(),
            camera: camera.enabled,
            camera_transport: camera.transport(),
            usb_controller: self.usb_enabled(),
            usb_passthrough: usb
                .passthrough_ports()
                .into_iter()
                .map(|(port, vendor_id, product_id)| UsbPrivacy {
                    port,
                    vendor_id,
                    product_id,
                })
                .collect(),
            shares: self
                .shares
                .iter()
                .map(|share| SharePrivacy {
                    tag: share.tag.clone(),
                    host_path: share.host_path.display().to_string(),
                    read_only: share.read_only,
                })
                .collect(),
            clipboard,
            printing,
            printers,
        };
        tracing::debug!(
            target: "ternvale::config",
            microphone = snapshot.microphone,
            camera = snapshot.camera,
            camera_transport = snapshot.camera_transport.as_str(),
            usb = snapshot.usb_passthrough.len(),
            shares = snapshot.shares.len(),
            clipboard = snapshot.clipboard,
            printing = snapshot.printing,
            printers = snapshot.printers.len(),
            "privacy snapshot"
        );
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use crate::VmConfig;

    #[test]
    fn defaults_are_off_except_usb_controller() {
        let text =
            "name = \"p\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n";
        let config = VmConfig::parse(text).expect("parse");
        let snap = config.privacy_snapshot();
        assert!(!snap.microphone);
        assert!(!snap.camera);
        assert!(snap.usb_controller);
        assert!(snap.usb_passthrough.is_empty());
        assert!(snap.shares.is_empty());
        assert!(!snap.clipboard);
        assert!(!snap.printing);
        assert!(snap.printers.is_empty());
    }
}
