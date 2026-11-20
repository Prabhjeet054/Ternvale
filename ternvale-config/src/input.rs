//! `[input]`: host keyboard mapping for guests.

use serde::{Deserialize, Serialize};

use crate::vm::VmConfig;

/// How the host Command keys are presented to the guest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CmdMap {
    /// Command stays `KEY_LEFTMETA` / `KEY_RIGHTMETA` (default).
    #[default]
    None,
    /// Command becomes Ctrl (`KEY_LEFTCTRL` / `KEY_RIGHTCTRL`).
    CmdCtrl,
    /// Command becomes Alt (`KEY_LEFTALT` / `KEY_RIGHTALT`).
    CmdAlt,
}

impl CmdMap {
    /// Config / log name.
    #[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::CmdCtrl => "cmd-ctrl",
            Self::CmdAlt => "cmd-alt",
        }
    }
}

/// The `[input]` table. Omitted: Command is Meta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct InputSection {
    /// Command key mapping. Default `none`.
    #[serde(default, skip_serializing_if = "cmd_map_is_default")]
    pub cmd_map: CmdMap,
}

fn cmd_map_is_default(value: &CmdMap) -> bool {
    *value == CmdMap::None
}

impl VmConfig {
    /// `[input]`, or defaults when the table is absent.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name))]
    pub fn input_section(&self) -> InputSection {
        self.input.clone().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::{CmdMap, InputSection};

    #[test]
    fn cmd_map_defaults_none() {
        assert_eq!(InputSection::default().cmd_map, CmdMap::None);
        assert_eq!(CmdMap::CmdCtrl.as_str(), "cmd-ctrl");
    }
}
