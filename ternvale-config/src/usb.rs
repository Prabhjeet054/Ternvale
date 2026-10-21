//! `[usb]`: virtual xHCI host with boot keyboard and mouse HID devices.

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::VmConfig;

/// The `[usb]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsbSection {
    /// `false`: no xHCI MMIO device or ACPI/DTB node. Default `true`.
    #[serde(default = "usb_enabled_default", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

impl Default for UsbSection {
    fn default() -> Self {
        Self { enabled: true }
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
        tracing::debug!(
            target: "ternvale::config",
            enabled = self.enabled,
            "accepted usb section"
        );
        Ok(())
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
