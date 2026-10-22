//! `.tstack` manifest: base raw image + ordered delta paths.

use std::path::{Path, PathBuf};

use crate::format::{STACK_MAGIC, STACK_VERSION};
use crate::SnapshotError;

/// Parsed stack manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackManifest {
    /// Host path of the read-only (or shared) base raw image.
    pub base: PathBuf,
    /// Delta layers oldest → newest.
    pub deltas: Vec<PathBuf>,
}

impl StackManifest {
    /// Load `path` (magic `TVST`, version 1).
    #[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(path = %path.display()))]
    pub fn load(path: &Path) -> Result<Self, SnapshotError> {
        let bytes = std::fs::read(path).map_err(|source| SnapshotError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        Self::parse(&bytes, dir, path)
    }

    fn parse(bytes: &[u8], dir: &Path, path: &Path) -> Result<Self, SnapshotError> {
        if bytes.len() < 12 {
            return Err(SnapshotError::Truncated {
                what: "stack manifest",
            });
        }
        if bytes[0..4] != STACK_MAGIC {
            return Err(SnapshotError::BadMagic {
                path: Some(path.to_path_buf()),
                expected: "TVST",
            });
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().expect("ver"));
        if version != STACK_VERSION {
            return Err(SnapshotError::UnsupportedVersion {
                kind: "stack",
                found: version,
                supported: STACK_VERSION,
            });
        }
        let base_len = u32::from_le_bytes(bytes[8..12].try_into().expect("base_len")) as usize;
        let mut off = 12usize;
        if bytes.len() < off + base_len + 4 {
            return Err(SnapshotError::Truncated {
                what: "stack base path",
            });
        }
        let base_rel = std::str::from_utf8(&bytes[off..off + base_len]).map_err(|_| {
            SnapshotError::InvalidManifest {
                path: path.to_path_buf(),
                detail: "base path is not UTF-8".into(),
            }
        })?;
        off += base_len;
        let count = u32::from_le_bytes(bytes[off..off + 4].try_into().expect("count")) as usize;
        off += 4;
        let base = Path::new(base_rel);
        let base = if base.is_absolute() {
            base.to_path_buf()
        } else {
            dir.join(base)
        };
        let mut deltas = Vec::with_capacity(count);
        for _ in 0..count {
            if bytes.len() < off + 4 {
                return Err(SnapshotError::Truncated {
                    what: "stack delta entry",
                });
            }
            let len = u32::from_le_bytes(bytes[off..off + 4].try_into().expect("dlen")) as usize;
            off += 4;
            if bytes.len() < off + len {
                return Err(SnapshotError::Truncated {
                    what: "stack delta path",
                });
            }
            let rel = std::str::from_utf8(&bytes[off..off + len]).map_err(|_| {
                SnapshotError::InvalidManifest {
                    path: path.to_path_buf(),
                    detail: "delta path is not UTF-8".into(),
                }
            })?;
            off += len;
            deltas.push(dir.join(rel));
        }
        tracing::info!(
            target: "ternvale::snapshot",
            base = %base.display(),
            deltas = deltas.len(),
            "loaded stack manifest"
        );
        Ok(Self { base, deltas })
    }

    /// Write manifest bytes (paths stored relative to `dir`).
    #[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all)]
    pub fn encode(&self, dir: &Path) -> Result<Vec<u8>, SnapshotError> {
        let base_rel = relative_path(dir, &self.base)?;
        let mut out = Vec::new();
        out.extend_from_slice(&STACK_MAGIC);
        out.extend_from_slice(&STACK_VERSION.to_le_bytes());
        out.extend_from_slice(&(base_rel.len() as u32).to_le_bytes());
        out.extend_from_slice(base_rel.as_bytes());
        out.extend_from_slice(&(self.deltas.len() as u32).to_le_bytes());
        for delta in &self.deltas {
            let rel = relative_path(dir, delta)?;
            out.extend_from_slice(&(rel.len() as u32).to_le_bytes());
            out.extend_from_slice(rel.as_bytes());
        }
        Ok(out)
    }

    /// Persist manifest to `path`.
    #[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(path = %path.display()))]
    pub fn save(&self, path: &Path) -> Result<(), SnapshotError> {
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let bytes = self.encode(dir)?;
        std::fs::write(path, bytes).map_err(|source| SnapshotError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }
}

fn relative_path(dir: &Path, target: &Path) -> Result<String, SnapshotError> {
    if target.is_relative() {
        return Ok(target.to_string_lossy().into_owned());
    }
    let dir = std::fs::canonicalize(dir).map_err(|source| SnapshotError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let target = std::fs::canonicalize(target).map_err(|source| SnapshotError::Io {
        path: target.to_path_buf(),
        source,
    })?;
    if let Ok(rel) = target.strip_prefix(&dir) {
        return Ok(rel.to_string_lossy().into_owned());
    }
    // Linked clones: shared base may live outside the clone's `disks/` directory.
    Ok(target.to_string_lossy().into_owned())
}
