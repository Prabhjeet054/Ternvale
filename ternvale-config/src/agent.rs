//! `[agent]` section: host-side policy for the guest agent.

use serde::{Deserialize, Serialize};

/// Guest-agent policy. Omitted means every flag is off.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSection {
    /// Allow `exec` and `cp` over the authenticated control socket.
    /// Default false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_exec: bool,
}

impl AgentSection {
    /// True when this is the all-defaults section (omitted from TOML).
    #[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
    pub fn is_default(&self) -> bool {
        !self.allow_exec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_exec_defaults_false_and_unknown_keys_fail() {
        let section: AgentSection = toml::from_str("").expect("empty");
        assert!(!section.allow_exec);
        assert!(section.is_default());
        let on: AgentSection = toml::from_str("allow_exec = true").expect("on");
        assert!(on.allow_exec);
        assert!(toml::from_str::<AgentSection>("shell = true").is_err());
    }
}
