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
    /// Host microphone into the guest. Default `false` (Step 111 permission
    /// model: opt-in per VM, macOS TCC prompt, visible in the UI).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub input: bool,
}

impl Default for AudioSection {
    fn default() -> Self {
        Self {
            enabled: true,
            input: false,
        }
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

    /// Host microphone capture. Off unless `[audio] input = true`.
    pub fn audio_input_enabled(&self) -> bool {
        self.audio_enabled() && self.audio_section().input
    }
}

#[cfg(test)]
mod tests {
    use super::AudioSection;

    #[test]
    fn microphone_defaults_off() {
        let audio = AudioSection::default();
        assert!(audio.enabled);
        assert!(!audio.input);
    }
}
