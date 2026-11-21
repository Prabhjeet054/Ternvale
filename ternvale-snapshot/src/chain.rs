//! Snapshot chain list with versioning and SHA-256 integrity.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::SnapshotError;

/// Chain file magic.
pub const CHAIN_MAGIC: &str = "TVSC";
/// Chain file version.
pub const CHAIN_VERSION: u32 = 1;
const CHAIN_FILE: &str = "chain.json";

/// One node in a VM's snapshot chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainNode {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub ram_sha256: String,
    pub ram_policy: String,
}

/// Versioned chain document (`chain.json` in the VM snapshot directory).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotChain {
    pub magic: String,
    pub version: u32,
    pub nodes: Vec<ChainNode>,
}

impl SnapshotChain {
    /// Empty chain.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            magic: CHAIN_MAGIC.into(),
            version: CHAIN_VERSION,
            nodes: Vec::new(),
        }
    }

    /// Load from `dir/chain.json`, or empty if missing.
    #[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(dir = %dir.display()))]
    pub fn load(dir: &Path) -> Result<Self, SnapshotError> {
        let path = dir.join(CHAIN_FILE);
        if !path.is_file() {
            return Ok(Self::empty());
        }
        let bytes = fs::read(&path).map_err(|source| SnapshotError::Io {
            path: path.clone(),
            source,
        })?;
        let chain: Self =
            serde_json::from_slice(&bytes).map_err(|error| SnapshotError::InvalidManifest {
                path: path.clone(),
                detail: format!("parse chain.json: {error}"),
            })?;
        if chain.magic != CHAIN_MAGIC {
            return Err(SnapshotError::BadMagic {
                path: Some(path),
                expected: CHAIN_MAGIC,
            });
        }
        if chain.version != CHAIN_VERSION {
            return Err(SnapshotError::UnsupportedVersion {
                kind: "snapshot-chain",
                found: chain.version,
                supported: CHAIN_VERSION,
            });
        }
        Ok(chain)
    }

    /// Write `dir/chain.json`.
    #[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(dir = %dir.display()))]
    pub fn save(&self, dir: &Path) -> Result<(), SnapshotError> {
        fs::create_dir_all(dir).map_err(|source| SnapshotError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = dir.join(CHAIN_FILE);
        let json =
            serde_json::to_vec_pretty(self).map_err(|error| SnapshotError::InvalidRequest {
                detail: format!("serialize chain: {error}"),
            })?;
        fs::write(&path, json).map_err(|source| SnapshotError::Io { path, source })?;
        Ok(())
    }

    /// Record or replace a node.
    pub fn upsert(&mut self, node: ChainNode) {
        if let Some(existing) = self.nodes.iter_mut().find(|n| n.label == node.label) {
            *existing = node;
        } else {
            self.nodes.push(node);
        }
    }

    /// Walk `label` to the base, parent-first. Names a missing/corrupt link.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::snapshot",
        skip_all,
        fields(label)
    )]
    pub fn lineage(&self, label: &str) -> Result<Vec<&ChainNode>, SnapshotError> {
        let mut out = Vec::new();
        let mut current = Some(label.to_string());
        let mut guard = 0u32;
        while let Some(name) = current {
            guard += 1;
            if guard > 64 {
                return Err(SnapshotError::InvalidRequest {
                    detail: format!("snapshot chain loop involving `{label}`"),
                });
            }
            let node = self.nodes.iter().find(|n| n.label == name).ok_or_else(|| {
                SnapshotError::InvalidManifest {
                    path: PathBuf::from(name.clone()),
                    detail: format!("missing snapshot chain link `{name}`"),
                }
            })?;
            current = node.parent.clone();
            out.push(node);
        }
        out.reverse();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lineage_names_missing_link() {
        let mut chain = SnapshotChain::empty();
        chain.upsert(ChainNode {
            label: "a".into(),
            parent: None,
            ram_sha256: "aa".into(),
            ram_policy: "full".into(),
        });
        chain.upsert(ChainNode {
            label: "c".into(),
            parent: Some("b".into()),
            ram_sha256: "cc".into(),
            ram_policy: "incremental".into(),
        });
        let err = chain.lineage("c").expect_err("missing");
        assert!(err.to_string().contains("`b`"), "{err}");
    }
}
