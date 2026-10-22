//! `[shares]`: host directories exported to the guest over virtio-9p.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Maximum mount tag length (virtio-9p config).
pub const MAX_TAG_LEN: usize = 36;
/// Maximum guest mount path length in agent messages.
pub const MAX_MOUNT_PATH: usize = 256;

/// One shared folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Share {
    /// Host directory to export (must exist at boot).
    pub host_path: PathBuf,
    /// virtio-9p mount tag (guest `mount -t 9p` tag=…).
    pub tag: String,
    /// Where the guest agent should mount the share (Linux).
    #[serde(default = "default_mount_point")]
    pub mount_point: String,
}

fn default_mount_point() -> String {
    "/mnt/ternvale".to_string()
}

impl Share {
    /// Validate tag, paths, and mount point.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(tag = %self.tag))]
    pub fn validate(&self) -> Result<(), crate::ConfigError> {
        if self.tag.is_empty() || self.tag.len() > MAX_TAG_LEN {
            return Err(crate::ConfigError::InvalidShare {
                field: "shares.tag",
                reason: format!("length must be 1..={MAX_TAG_LEN}"),
            });
        }
        if self.tag.contains('\0') {
            return Err(crate::ConfigError::InvalidShare {
                field: "shares.tag",
                reason: "must not contain NUL".into(),
            });
        }
        if !self.host_path.is_dir() {
            return Err(crate::ConfigError::InvalidShare {
                field: "shares.host_path",
                reason: format!("{} is not a directory", self.host_path.display()),
            });
        }
        if self.mount_point.is_empty() || self.mount_point.len() > MAX_MOUNT_PATH {
            return Err(crate::ConfigError::InvalidShare {
                field: "shares.mount_point",
                reason: format!("length must be 1..={MAX_MOUNT_PATH}"),
            });
        }
        if !self.mount_point.starts_with('/') {
            return Err(crate::ConfigError::InvalidShare {
                field: "shares.mount_point",
                reason: "must be an absolute path".into(),
            });
        }
        tracing::debug!(
            target: "ternvale::config",
            host = %self.host_path.display(),
            mount = %self.mount_point,
            "accepted share"
        );
        Ok(())
    }
}
