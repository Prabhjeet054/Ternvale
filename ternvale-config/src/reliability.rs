//! `[reliability]`: guest panic, watchdog, and VMM-process crash policy.

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// Default RAM dump cap for `dump-and-pause` (guest RAM can contain secrets).
pub const DEFAULT_DUMP_RAM_MAX_MIB: u64 = 256;
/// Largest dump we will write (8 GiB).
pub const MAX_DUMP_RAM_MAX_MIB: u64 = 8 * 1024;

/// What the host does when the guest signals a panic (`pvpanic`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnPanic {
    /// Park vCPUs; keep the process so the dump/UI can inspect state.
    #[default]
    Pause,
    /// PSCI `SYSTEM_RESET` analog (the VMM process exits reset).
    Restart,
    /// PSCI `SYSTEM_OFF`.
    Poweroff,
    /// Write a capped guest-RAM dump, then pause.
    DumpAndPause,
}

/// What the host does when the SBSA watchdog reaches WS1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnWatchdog {
    /// Reset the VM (PSCI `SYSTEM_RESET`).
    #[default]
    Reset,
    /// Power the VM off.
    Poweroff,
    /// Park vCPUs.
    Pause,
}

/// What the app/daemon supervisor does when the VMM process dies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnVmmCrash {
    /// Restart with exponential backoff; stop after 3 crashes in 5 minutes.
    #[default]
    Restart,
    /// Leave the VM stopped and notify.
    Stop,
}

/// `[reliability]` table. Omitted fields use the defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReliabilitySection {
    /// Guest `pvpanic` (Linux sysrq, Windows virtio-win pvpanic).
    #[serde(default, skip_serializing_if = "is_pause")]
    pub on_panic: OnPanic,
    /// SBSA generic watchdog WS1.
    #[serde(default, skip_serializing_if = "is_wd_reset")]
    pub on_watchdog: OnWatchdog,
    /// Supervisor policy when the VMM helper process exits unexpectedly.
    #[serde(default, skip_serializing_if = "is_vmm_restart")]
    pub on_vmm_crash: OnVmmCrash,
    /// Cap for `dump-and-pause` RAM files, in MiB.
    #[serde(
        default = "default_dump_ram_max_mib",
        skip_serializing_if = "is_default_dump_ram_max_mib"
    )]
    pub dump_ram_max_mib: u64,
}

impl Default for ReliabilitySection {
    fn default() -> Self {
        Self {
            on_panic: OnPanic::Pause,
            on_watchdog: OnWatchdog::Reset,
            on_vmm_crash: OnVmmCrash::Restart,
            dump_ram_max_mib: DEFAULT_DUMP_RAM_MAX_MIB,
        }
    }
}

fn is_pause(value: &OnPanic) -> bool {
    *value == OnPanic::Pause
}

fn is_wd_reset(value: &OnWatchdog) -> bool {
    *value == OnWatchdog::Reset
}

fn is_vmm_restart(value: &OnVmmCrash) -> bool {
    *value == OnVmmCrash::Restart
}

fn default_dump_ram_max_mib() -> u64 {
    DEFAULT_DUMP_RAM_MAX_MIB
}

fn is_default_dump_ram_max_mib(value: &u64) -> bool {
    *value == DEFAULT_DUMP_RAM_MAX_MIB
}

impl OnPanic {
    /// Wire / log name.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Restart => "restart",
            Self::Poweroff => "poweroff",
            Self::DumpAndPause => "dump-and-pause",
        }
    }
}

impl OnWatchdog {
    /// Wire / log name.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reset => "reset",
            Self::Poweroff => "poweroff",
            Self::Pause => "pause",
        }
    }
}

impl OnVmmCrash {
    /// Wire / log name.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Restart => "restart",
            Self::Stop => "stop",
        }
    }
}

impl ReliabilitySection {
    /// True when every field is the default (omit the table from TOML).
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// Range-check `dump_ram_max_mib`.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(1..=MAX_DUMP_RAM_MAX_MIB).contains(&self.dump_ram_max_mib) {
            tracing::error!(
                target: "ternvale::config",
                mib = self.dump_ram_max_mib,
                "reliability.dump_ram_max_mib out of range"
            );
            return Err(ConfigError::InvalidReliability {
                field: "reliability.dump_ram_max_mib",
                reason: format!(
                    "must be 1..={MAX_DUMP_RAM_MAX_MIB}, got {}",
                    self.dump_ram_max_mib
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
    fn defaults_and_kebab_case() {
        let plain = ReliabilitySection::default();
        assert!(plain.is_default());
        assert_eq!(OnPanic::DumpAndPause.as_str(), "dump-and-pause");
        assert_eq!(OnWatchdog::Reset.as_str(), "reset");
        let parsed: ReliabilitySection = toml::from_str(
            r#"
on_panic = "dump-and-pause"
on_watchdog = "pause"
on_vmm_crash = "stop"
dump_ram_max_mib = 64
"#,
        )
        .expect("parse");
        assert_eq!(parsed.on_panic, OnPanic::DumpAndPause);
        assert_eq!(parsed.on_watchdog, OnWatchdog::Pause);
        assert_eq!(parsed.on_vmm_crash, OnVmmCrash::Stop);
        assert_eq!(parsed.dump_ram_max_mib, 64);
    }

    #[test]
    fn zero_dump_cap_is_rejected() {
        let bad = ReliabilitySection {
            dump_ram_max_mib: 0,
            ..ReliabilitySection::default()
        };
        assert!(bad.validate().is_err());
    }
}
