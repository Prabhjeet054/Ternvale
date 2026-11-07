//! `[shares]`: host directories exported to the guest over virtio-fs.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Maximum mount tag length (`virtio_fs_config.tag`).
pub const MAX_TAG_LEN: usize = 36;
/// Maximum guest mount path length in agent messages.
pub const MAX_MOUNT_PATH: usize = 256;

/// One shared folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Share {
    /// Host directory to export (must exist at boot).
    pub host_path: PathBuf,
    /// virtio-fs mount tag (guest `mount -t virtiofs` source).
    pub tag: String,
    /// Where the guest agent should mount the share (Linux path or `Z:`).
    #[serde(default = "default_mount_point")]
    pub mount_point: String,
    /// Guest writes fail with `EROFS`.
    #[serde(default)]
    pub read_only: bool,
}

fn default_mount_point() -> String {
    "/mnt/ternvale".to_string()
}

/// Linux `/path` or Windows `X:` / `X:\…`.
#[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
pub fn is_guest_mount_point(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    let bytes = path.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return false;
    }
    let rest = &path[2..];
    rest.is_empty() || rest.starts_with('\\') || rest.starts_with('/')
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
        if !is_guest_mount_point(&self.mount_point) {
            return Err(crate::ConfigError::InvalidShare {
                field: "shares.mount_point",
                reason: "must be an absolute Unix path or a Windows drive letter (Z:)".into(),
            });
        }
        tracing::debug!(
            target: "ternvale::config",
            host = %self.host_path.display(),
            mount = %self.mount_point,
            read_only = self.read_only,
            "accepted share"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::is_guest_mount_point;

    #[test]
    fn guest_mount_points_unix_and_windows() {
        assert!(is_guest_mount_point("/mnt/ternvale"));
        assert!(is_guest_mount_point("Z:"));
        assert!(is_guest_mount_point("z:\\"));
        assert!(is_guest_mount_point("D:/share"));
        assert!(!is_guest_mount_point("relative"));
        assert!(!is_guest_mount_point(""));
    }
}
