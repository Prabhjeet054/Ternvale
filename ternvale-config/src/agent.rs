//! `[agent]` section: host-side policy for the guest agent.

use serde::{Deserialize, Serialize};

/// Default URL schemes for guest → host open (never `file:`).
pub const DEFAULT_URL_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

/// Guest-agent policy. Omitted means every flag is off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSection {
    /// Allow `exec` and `cp` over the authenticated control socket.
    /// Default false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_exec: bool,
    /// Generate `~/Applications/Ternvale Apps` launchers for this VM.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub launchers: bool,
    /// Finder “Open With” / host files into a guest app (share path only).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub open_in_guest: bool,
    /// Guest URLs/files forwarded to the macOS default handler.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub open_on_host: bool,
    /// Allowed URL schemes for `open_on_host` (`file:` is never allowed).
    #[serde(
        default = "default_url_schemes",
        skip_serializing_if = "is_default_url_schemes"
    )]
    pub url_schemes: Vec<String>,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            allow_exec: false,
            launchers: false,
            open_in_guest: false,
            open_on_host: false,
            url_schemes: default_url_schemes(),
        }
    }
}

fn default_url_schemes() -> Vec<String> {
    DEFAULT_URL_SCHEMES
        .iter()
        .map(|scheme| (*scheme).to_string())
        .collect()
}

fn is_default_url_schemes(schemes: &[String]) -> bool {
    schemes.len() == DEFAULT_URL_SCHEMES.len()
        && schemes
            .iter()
            .zip(DEFAULT_URL_SCHEMES)
            .all(|(got, want)| got.eq_ignore_ascii_case(want))
}

impl AgentSection {
    /// True when this is the all-defaults section (omitted from TOML).
    #[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
    pub fn is_default(&self) -> bool {
        !self.allow_exec
            && !self.launchers
            && !self.open_in_guest
            && !self.open_on_host
            && is_default_url_schemes(&self.url_schemes)
    }

    /// Whether `scheme` is on the allowlist. `file` is always refused.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(scheme))]
    pub fn scheme_allowed(&self, scheme: &str) -> bool {
        if scheme.eq_ignore_ascii_case("file") {
            tracing::warn!(target: "ternvale::config", "file: URL scheme is never allowed");
            return false;
        }
        self.url_schemes
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(scheme))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_exec_defaults_false_and_unknown_keys_fail() {
        let section: AgentSection = toml::from_str("").expect("empty");
        assert!(!section.allow_exec);
        assert!(!section.launchers);
        assert!(!section.open_in_guest);
        assert!(!section.open_on_host);
        assert_eq!(section.url_schemes, default_url_schemes());
        assert!(section.is_default());
        let on: AgentSection = toml::from_str("allow_exec = true").expect("on");
        assert!(on.allow_exec);
        assert!(!on.is_default());
        assert!(toml::from_str::<AgentSection>("shell = true").is_err());
        let apps: AgentSection = toml::from_str(
            "launchers = true\nopen_in_guest = true\nopen_on_host = true\nurl_schemes = [\"https\"]\n",
        )
        .expect("apps");
        assert!(apps.launchers && apps.open_in_guest && apps.open_on_host);
        assert!(apps.scheme_allowed("https"));
        assert!(!apps.scheme_allowed("http"));
        assert!(!apps.scheme_allowed("file"));
        assert!(!AgentSection::default().scheme_allowed("file"));
        assert!(AgentSection::default().scheme_allowed("mailto"));
    }
}
