//! `[usb]`: virtual xHCI host with boot keyboard and mouse HID devices.

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::vm::VmConfig;

/// Host USB device to expose through the guest xHCI controller (passthrough).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsbPassthroughEntry {
    /// USB vendor ID (`idVendor`).
    pub vendor_id: u16,
    /// USB product ID (`idProduct`).
    pub product_id: u16,
    /// Guest root port (1-based). Defaults to the next free passthrough port (3 or 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u8>,
}

/// The `[usb]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsbSection {
    /// `false`: no xHCI MMIO device or ACPI/DTB node. Default `true`.
    #[serde(default = "usb_enabled_default", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Host devices forwarded to the guest when claimed at VM start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passthrough: Vec<UsbPassthroughEntry>,
}

impl Default for UsbSection {
    fn default() -> Self {
        Self {
            enabled: true,
            passthrough: Vec::new(),
        }
    }
}

fn usb_enabled_default() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

impl UsbSection {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        let mut next_port = 3u8;
        for entry in &self.passthrough {
            if entry.vendor_id == 0 && entry.product_id == 0 {
                return Err(ConfigError::InvalidUsb {
                    field: "usb.passthrough",
                    reason: "vendor_id and product_id cannot both be zero".into(),
                });
            }
            let port = entry.port.unwrap_or_else(|| {
                let p = next_port;
                next_port = next_port.saturating_add(1);
                p
            });
            if !(3..=4).contains(&port) {
                return Err(ConfigError::InvalidUsb {
                    field: "usb.passthrough.port",
                    reason: "passthrough ports must be 3 or 4 (ports 1–2 are virtual HID)".into(),
                });
            }
            tracing::debug!(
                target: "ternvale::config",
                vendor_id = format!("{:#06x}", entry.vendor_id),
                product_id = format!("{:#06x}", entry.product_id),
                port,
                "accepted usb passthrough entry"
            );
        }
        tracing::debug!(
            target: "ternvale::config",
            enabled = self.enabled,
            passthrough = self.passthrough.len(),
            "accepted usb section"
        );
        Ok(())
    }

    /// Resolved `(port, vendor_id, product_id)` tuples for the VMM.
    #[must_use]
    pub fn passthrough_ports(&self) -> Vec<(u8, u16, u16)> {
        let mut next_port = 3u8;
        self.passthrough
            .iter()
            .map(|entry| {
                let port = entry.port.unwrap_or_else(|| {
                    let p = next_port;
                    next_port = next_port.saturating_add(1);
                    p
                });
                (port, entry.vendor_id, entry.product_id)
            })
            .collect()
    }
}

impl VmConfig {
    /// `[usb]`, or defaults when the table is absent.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name))]
    pub fn usb_section(&self) -> UsbSection {
        self.usb.clone().unwrap_or_default()
    }

    /// False when `[usb] enabled = false`.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name))]
    pub fn usb_enabled(&self) -> bool {
        self.usb.as_ref().is_none_or(|usb| usb.enabled)
    }
}
