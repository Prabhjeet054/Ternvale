//! Versioned tools-ISO manifest (`MANIFEST.json`).
//!
//! Magic `TERNVALE-TOOLS` and integer `version` so a future format cannot be
//! mistaken for this one. Unknown versions are rejected.

use serde::{Deserialize, Serialize};

/// On-disk magic string.
pub const MAGIC: &str = "TERNVALE-TOOLS";
/// Current `MANIFEST.json` layout.
pub const VERSION: u32 = 1;

/// One file shipped on the ISO (paths relative to the volume root).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    /// POSIX-style relative path (`agent/ternvale-agent.exe`).
    pub path: String,
    /// Lower-case hex SHA-256.
    pub sha256: String,
    /// Size in bytes.
    pub bytes: u64,
}

/// Tools ISO contents the app compares to a running guest agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsManifest {
    /// [`MAGIC`].
    pub magic: String,
    /// [`VERSION`].
    pub version: u32,
    /// Tools pack version (Ternvale crate version when the ISO was built).
    pub tools_version: String,
    /// `ternvale-agent` crate / PE version.
    pub agent_version: String,
    /// virtio-win release label, if known (`0.1.302`).
    pub virtio_win: String,
    /// RFC 3339 timestamp.
    pub built: String,
    /// Every payload file except this manifest.
    pub files: Vec<ManifestFile>,
    /// Optional Ternvale guest-driver pack stamp (`tvgpu-<semver>`). Additive;
    /// older `MANIFEST.json` files omit it (serde default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drivers_version: Option<String>,
}

impl ToolsManifest {
    /// New v1 document.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn new(
        tools_version: String,
        agent_version: String,
        virtio_win: String,
        files: Vec<ManifestFile>,
    ) -> Self {
        Self {
            magic: MAGIC.to_string(),
            version: VERSION,
            tools_version,
            agent_version,
            virtio_win,
            built: chrono::Utc::now().to_rfc3339(),
            files,
            drivers_version: None,
        }
    }

    /// Reject unknown magic or version rather than guessing.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn parse(json: &str) -> Result<Self, String> {
        let parsed: Self = serde_json::from_str(json).map_err(|error| error.to_string())?;
        if parsed.magic != MAGIC {
            return Err(format!(
                "tools manifest magic {:?} is not {MAGIC:?}",
                parsed.magic
            ));
        }
        if parsed.version != VERSION {
            return Err(format!(
                "tools manifest version {} is not supported (this build reads {VERSION})",
                parsed.version
            ));
        }
        Ok(parsed)
    }

    /// `true` when the guest agent's version is not the one on this ISO.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn guest_outdated(&self, guest_agent_version: &str) -> bool {
        let guest = guest_agent_version.trim();
        let expected = self.agent_version.trim();
        if guest.is_empty() {
            return true;
        }
        let agent_old = guest != expected && !guest.contains(expected);
        let drivers_old = match self.drivers_version.as_deref() {
            Some(stamp) if !stamp.is_empty() => guest != stamp && !guest.contains(stamp),
            _ => false,
        };
        let outdated = agent_old || drivers_old;
        if outdated {
            tracing::info!(
                target: "ternvale::cli",
                guest,
                expected,
                drivers_version = self.drivers_version.as_deref().unwrap_or(""),
                tools_version = %self.tools_version,
                "guest tools are older than the tools ISO"
            );
        }
        outdated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_unknown_version_and_magic() {
        let good = ToolsManifest::new("0.1.0".into(), "0.1.0".into(), "0.1.302".into(), Vec::new());
        let json = serde_json::to_string(&good).expect("json");
        assert_eq!(ToolsManifest::parse(&json).expect("ok").version, VERSION);
        let bad_ver = json.replace("\"version\":1", "\"version\":99");
        let error = ToolsManifest::parse(&bad_ver).expect_err("version");
        assert!(error.contains("99"), "{error}");
        let bad_magic = json.replace(MAGIC, "NOPE");
        let error = ToolsManifest::parse(&bad_magic).expect_err("magic");
        assert!(error.contains("magic"), "{error}");
    }

    #[test]
    fn guest_outdated_when_agent_version_differs() {
        let man = ToolsManifest::new("0.1.0".into(), "0.1.0".into(), "0.1.302".into(), Vec::new());
        assert!(man.guest_outdated(""));
        assert!(man.guest_outdated("ternvale-agent 0.0.9"));
        assert!(!man.guest_outdated("0.1.0"));
        assert!(!man.guest_outdated("ternvale-agent 0.1.0"));
    }

    #[test]
    fn drivers_version_flags_guests_that_do_not_advertise_the_stamp() {
        let mut man =
            ToolsManifest::new("0.1.0".into(), "0.1.0".into(), "0.1.302".into(), Vec::new());
        man.drivers_version = Some("tvgpu-0.1.0".into());
        assert!(man.guest_outdated("ternvale-agent 0.1.0"));
        assert!(!man.guest_outdated("ternvale-agent 0.1.0 tvgpu-0.1.0"));
        let json = serde_json::to_string(&man).expect("json");
        assert!(json.contains("drivers_version"));
        let parsed = ToolsManifest::parse(&json).expect("parse");
        assert_eq!(parsed.drivers_version.as_deref(), Some("tvgpu-0.1.0"));
        let old = r#"{"magic":"TERNVALE-TOOLS","version":1,"tools_version":"0.1.0","agent_version":"0.1.0","virtio_win":"0.1.302","built":"t","files":[]}"#;
        let without = ToolsManifest::parse(old).expect("compat");
        assert_eq!(without.drivers_version, None);
        assert!(!without.guest_outdated("ternvale-agent 0.1.0"));
    }
}
