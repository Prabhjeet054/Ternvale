//! `[audio]`: virtio-sound output to the host speakers.

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::vm::VmConfig;

/// The `[audio]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioSection {
    /// `false`: no virtio-sound device. Default `true`.
    #[serde(default = "audio_enabled_default", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

impl Default for AudioSection {
    fn default() -> Self {
        Self { enabled: true }
    }
}

fn audio_enabled_default() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

impl AudioSection {
    /// No extra validation today.
    pub fn validate(&self) -> Result<(), ConfigError> {
        Ok(())
    }
}

impl VmConfig {
    /// `[audio]` or defaults.
    pub fn audio_section(&self) -> AudioSection {
        self.audio.clone().unwrap_or_default()
    }

    /// Whether virtio-sound is attached.
    pub fn audio_enabled(&self) -> bool {
        self.audio_section().enabled
    }
}
