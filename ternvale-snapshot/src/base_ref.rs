//! Reference counts for shared read-only disk base images (`TVRC` sidecar files).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::SnapshotError;

const REF_MAGIC: &str = "TVRC";
const REF_VERSION: u32 = 1;

/// Parsed refcount sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseRefFile {
    pub magic: String,
    pub version: u32,
    /// Canonical base image path this file tracks.
    pub base: PathBuf,
    /// Sum of outstanding holds (each acquire +1, release -1).
    pub count: u64,
    /// Per-VM (or holder) outstanding holds.
    pub holders: BTreeMap<String, u64>,
}

impl BaseRefFile {
    fn new(base: PathBuf) -> Self {
        Self {
            magic: REF_MAGIC.to_string(),
            version: REF_VERSION,
            base,
            count: 0,
            holders: BTreeMap::new(),
        }
    }

    fn validate(&self, path: &Path) -> Result<(), SnapshotError> {
        if self.magic != REF_MAGIC {
            return Err(SnapshotError::BadMagic {
                path: Some(path.to_path_buf()),
                expected: REF_MAGIC,
            });
        }
        if self.version != REF_VERSION {
            return Err(SnapshotError::UnsupportedVersion {
                kind: "base-ref",
                found: self.version,
                supported: REF_VERSION,
            });
        }
        Ok(())
    }
}

/// Directory for `*.tvref` files (`~/Library/Application Support/Ternvale/refs`).
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all)]
pub fn refs_dir() -> Result<PathBuf, SnapshotError> {
    if let Some(dir) = std::env::var_os("TERNVALE_REFS_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| SnapshotError::InvalidRequest {
        detail: "HOME is not set; cannot find the refs directory".to_string(),
    })?;
    Ok(PathBuf::from(home).join("Library/Application Support/Ternvale/refs"))
}

/// Canonical path used as the refcount key.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(path = %path.display()))]
pub fn canonical_base(path: &Path) -> Result<PathBuf, SnapshotError> {
    std::fs::canonicalize(path).map_err(|source| SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Sidecar path for a canonical base image.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all)]
pub fn sidecar_path(canonical_base: &Path) -> Result<PathBuf, SnapshotError> {
    let dir = refs_dir()?;
    let name = format!(
        "{}.tvref",
        sha256_path(canonical_base.as_os_str().as_encoded_bytes())
    );
    Ok(dir.join(name))
}

fn sha256_path(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Register a holder on `base`. Returns the new total count.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(holder, base = %base.display()))]
pub fn acquire(base: &Path, holder: &str) -> Result<u64, SnapshotError> {
    validate_holder(holder)?;
    let canonical = canonical_base(base)?;
    let path = sidecar_path(&canonical)?;
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).map_err(|source| {
        SnapshotError::Io {
            path: path.clone(),
            source,
        }
    })?;
    let mut file = lock_ref_file(&path)?;
    let mut state = read_ref_file(&path, &canonical)?;
    let before = state.count;
    state.holders.insert(
        holder.to_string(),
        state.holders.get(holder).copied().unwrap_or(0) + 1,
    );
    state.count += 1;
    write_ref_file(&mut file, &path, &state)?;
    tracing::info!(
        target: "ternvale::snapshot",
        base = %canonical.display(),
        holder,
        before,
        after = state.count,
        "shared disk base refcount incremented"
    );
    Ok(state.count)
}

/// Drop one hold from `holder`. Returns the new total count.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(holder, base = %base.display()))]
pub fn release(base: &Path, holder: &str) -> Result<u64, SnapshotError> {
    validate_holder(holder)?;
    let canonical = canonical_base(base)?;
    let path = sidecar_path(&canonical)?;
    let mut file = lock_ref_file(&path)?;
    let mut state = read_ref_file(&path, &canonical)?;
    let before = state.count;
    let Some(cur) = state.holders.get_mut(holder) else {
        tracing::warn!(
            target: "ternvale::snapshot",
            base = %canonical.display(),
            holder,
            "refcount release with no prior hold"
        );
        return Ok(state.count);
    };
    if *cur == 0 {
        return Ok(state.count);
    }
    *cur -= 1;
    if *cur == 0 {
        state.holders.remove(holder);
    }
    state.count = state.count.saturating_sub(1);
    write_ref_file(&mut file, &path, &state)?;
    tracing::info!(
        target: "ternvale::snapshot",
        base = %canonical.display(),
        holder,
        before,
        after = state.count,
        "shared disk base refcount decremented"
    );
    Ok(state.count)
}

/// Current refcount for `base` (0 when no sidecar exists).
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(base = %base.display()))]
pub fn count(base: &Path) -> Result<u64, SnapshotError> {
    let canonical = canonical_base(base)?;
    let path = sidecar_path(&canonical)?;
    if !path.is_file() {
        return Ok(0);
    }
    let state = read_ref_file(&path, &canonical)?;
    Ok(state.count)
}

/// Whether `base` must not be opened writable (any outstanding refcount).
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(path = %path.display()))]
pub fn is_write_protected(path: &Path) -> Result<bool, SnapshotError> {
    Ok(count(path)? > 0)
}

/// Whether `holder` already has at least one hold on `base`.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(holder, base = %base.display()))]
pub fn has_holder(base: &Path, holder: &str) -> Result<bool, SnapshotError> {
    let canonical = canonical_base(base)?;
    let path = sidecar_path(&canonical)?;
    if !path.is_file() {
        return Ok(false);
    }
    let state = read_ref_file(&path, &canonical)?;
    Ok(state.holders.get(holder).copied().unwrap_or(0) > 0)
}

fn validate_holder(holder: &str) -> Result<(), SnapshotError> {
    let ok = !holder.is_empty()
        && holder
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if !ok {
        return Err(SnapshotError::InvalidRequest {
            detail: format!("invalid holder name {holder:?}"),
        });
    }
    Ok(())
}

fn lock_ref_file(path: &Path) -> Result<File, SnapshotError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|source| SnapshotError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn read_ref_file(path: &Path, canonical: &Path) -> Result<BaseRefFile, SnapshotError> {
    if !path.is_file() {
        return Ok(BaseRefFile::new(canonical.to_path_buf()));
    }
    let mut file =
        OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|source| SnapshotError::Io {
                path: path.to_path_buf(),
                source,
            })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| SnapshotError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.is_empty() {
        return Ok(BaseRefFile::new(canonical.to_path_buf()));
    }
    let state: BaseRefFile =
        serde_json::from_slice(&bytes).map_err(|error| SnapshotError::InvalidManifest {
            path: path.to_path_buf(),
            detail: format!("parse tvref: {error}"),
        })?;
    state.validate(path)?;
    Ok(state)
}

fn write_ref_file(file: &mut File, path: &Path, state: &BaseRefFile) -> Result<(), SnapshotError> {
    let bytes =
        serde_json::to_vec_pretty(state).map_err(|error| SnapshotError::InvalidRequest {
            detail: format!("serialize tvref: {error}"),
        })?;
    file.set_len(0).map_err(|source| SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| SnapshotError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(&bytes).map_err(|source| SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_and_release_round_trip() {
        let dir = std::env::temp_dir().join(format!("tvref-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        // SAFETY: test-only env.
        unsafe { std::env::set_var("TERNVALE_REFS_DIR", dir.join("refs")) };
        let base = dir.join("base.img");
        std::fs::write(&base, &[0u8; 4096]).expect("base");
        assert_eq!(acquire(&base, "vm-a").expect("acq"), 1);
        assert_eq!(acquire(&base, "vm-b").expect("acq"), 2);
        assert!(is_write_protected(&base).expect("prot"));
        assert_eq!(release(&base, "vm-a").expect("rel"), 1);
        assert_eq!(release(&base, "vm-b").expect("rel"), 0);
        assert!(!is_write_protected(&base).expect("prot"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
