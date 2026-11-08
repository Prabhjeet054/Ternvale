//! `[power]`: host sleep/wake, quit, and window-hidden policy.

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// Default wait for `ternvale stop --graceful` before force-stop.
pub const DEFAULT_GRACEFUL_TIMEOUT_SECS: u64 = 30;
/// Longest graceful-stop wait.
pub const MAX_GRACEFUL_TIMEOUT_SECS: u64 = 600;

/// What `ternvale run` does on SIGINT/SIGTERM (app quit).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnQuit {
    /// Pulse the guest power button and wait, then force-stop.
    #[default]
    Shutdown,
    /// Pause vCPUs (CNTVCT hiding) and keep the process until a later stop.
    Suspend,
}

/// `[power]` table. Omitted fields use the defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowerSection {
    /// SIGINT/SIGTERM / app quit.
    #[serde(default, skip_serializing_if = "is_shutdown")]
    pub on_quit: OnQuit,
    /// Pause while the display window is hidden (`window-hidden` control cmd).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pause_when_hidden: bool,
    /// Implies [`Self::pause_when_hidden`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub low_power: bool,
    /// Seconds `graceful-stop` waits before force-stop.
    #[serde(
        default = "default_graceful_timeout_secs",
        skip_serializing_if = "is_default_graceful_timeout"
    )]
    pub graceful_timeout_secs: u64,
}

impl Default for PowerSection {
    fn default() -> Self {
        Self {
            on_quit: OnQuit::Shutdown,
            pause_when_hidden: false,
            low_power: false,
            graceful_timeout_secs: DEFAULT_GRACEFUL_TIMEOUT_SECS,
        }
    }
}

fn is_shutdown(on_quit: &OnQuit) -> bool {
    *on_quit == OnQuit::Shutdown
}

impl OnQuit {
    /// Wire / log name.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shutdown => "shutdown",
            Self::Suspend => "suspend",
        }
    }
}

fn default_graceful_timeout_secs() -> u64 {
    DEFAULT_GRACEFUL_TIMEOUT_SECS
}

fn is_default_graceful_timeout(value: &u64) -> bool {
    *value == DEFAULT_GRACEFUL_TIMEOUT_SECS
}

impl PowerSection {
    /// True when every field is the default (omit the table from TOML).
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// Pause-when-hidden, including [`Self::low_power`].
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn pause_when_hidden(&self) -> bool {
        self.pause_when_hidden || self.low_power
    }

    /// Range-check `graceful_timeout_secs`.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(1..=MAX_GRACEFUL_TIMEOUT_SECS).contains(&self.graceful_timeout_secs) {
            tracing::error!(
                target: "ternvale::config",
                secs = self.graceful_timeout_secs,
                "power.graceful_timeout_secs out of range"
            );
            return Err(ConfigError::InvalidPower {
                field: "power.graceful_timeout_secs",
                reason: format!(
                    "must be 1..={MAX_GRACEFUL_TIMEOUT_SECS}, got {}",
                    self.graceful_timeout_secs
                ),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_low_power_imply_pause_when_hidden() {
        let plain = PowerSection::default();
        assert!(plain.is_default());
        assert!(!plain.pause_when_hidden());
        let low = PowerSection {
            low_power: true,
            ..PowerSection::default()
        };
        assert!(low.pause_when_hidden());
        assert_eq!(OnQuit::Shutdown.as_str(), "shutdown");
    }

    #[test]
    fn timeout_zero_is_rejected() {
        let bad = PowerSection {
            graceful_timeout_secs: 0,
            ..PowerSection::default()
        };
        assert!(bad.validate().is_err());
    }
}
