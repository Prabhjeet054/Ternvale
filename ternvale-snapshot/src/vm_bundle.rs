//! Full-VM snapshot bundles (guest RAM + device metadata).
//!
//! v1 uses a **full RAM dump** per region (no memory COW). See `docs/VM_SNAPSHOT.md`.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::SnapshotError;

/// Magic in `meta.json` (`TVSB`).
pub const BUNDLE_MAGIC: &str = "TVSB";
/// Supported bundle format version.
pub const BUNDLE_VERSION: u32 = 1;

const META: &str = "meta.json";
const GIC: &str = "gic.bin";
const RAM: &str = "guest.ram";
const VCPUS: &str = "vcpus.json";
const DEVICES: &str = "devices.json";

/// On-disk metadata for one VM snapshot directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmSnapshotMeta {
    pub magic: String,
    pub version: u32,
    pub vm_name: String,
    pub label: String,
    pub created_unix_ms: u64,
    pub ram_base: u64,
    pub ram_size: u64,
    /// `"full"` today; future may add `"cow"`.
    pub ram_policy: String,
    pub gic_bytes: u64,
    pub vcpu_count: u32,
    /// Human-readable gaps (virtio queue state, etc.).
    pub limitations: Vec<String>,
}

/// Guest RAM plus GIC blob and JSON sidecars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmSnapshotPayload {
    pub meta: VmSnapshotMeta,
    pub gic: Vec<u8>,
    pub ram: Vec<u8>,
    pub vcpus_json: String,
    pub devices_json: String,
}

/// Build metadata for a new bundle.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(vm = vm_name, label))]
pub fn new_meta(
    vm_name: &str,
    label: &str,
    ram_base: u64,
    ram_size: u64,
    gic_len: usize,
    vcpu_count: u32,
) -> VmSnapshotMeta {
    let created_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    VmSnapshotMeta {
        magic: BUNDLE_MAGIC.to_string(),
        version: BUNDLE_VERSION,
        vm_name: vm_name.to_string(),
        label: label.to_string(),
        created_unix_ms,
        ram_base,
        ram_size,
        ram_policy: "full".to_string(),
        gic_bytes: u64::try_from(gic_len).unwrap_or(u64::MAX),
        vcpu_count,
        limitations: vec![
            "Guest RAM is a full dump (no memory COW yet).".to_string(),
            "Virtio queue and in-flight request state are not restored; disks use layered .tstack snapshots separately.".to_string(),
        ],
    }
}

/// Write a versioned bundle directory (created if missing).
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(dir = %dir.display()))]
pub fn write_bundle(dir: &Path, payload: &VmSnapshotPayload) -> Result<(), SnapshotError> {
    if payload.meta.magic != BUNDLE_MAGIC || payload.meta.version != BUNDLE_VERSION {
        return Err(SnapshotError::InvalidRequest {
            detail: "bundle meta magic/version mismatch".to_string(),
        });
    }
    fs::create_dir_all(dir).map_err(|source| io_err(dir, source))?;
    let meta_path = dir.join(META);
    let json = serde_json::to_vec_pretty(&payload.meta).map_err(|error| {
        SnapshotError::InvalidRequest {
            detail: format!("serialize meta: {error}"),
        }
    })?;
    write_bytes(meta_path, &json)?;
    write_bytes(dir.join(GIC), &payload.gic)?;
    write_bytes(dir.join(RAM), &payload.ram)?;
    write_bytes(dir.join(VCPUS), payload.vcpus_json.as_bytes())?;
    write_bytes(dir.join(DEVICES), payload.devices_json.as_bytes())?;
    tracing::info!(
        target: "ternvale::snapshot",
        dir = %dir.display(),
        ram_bytes = payload.ram.len(),
        gic_bytes = payload.gic.len(),
        vcpus = payload.meta.vcpu_count,
        "vm snapshot bundle written"
    );
    Ok(())
}

/// Read and validate a bundle directory.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(dir = %dir.display()))]
pub fn read_bundle(dir: &Path) -> Result<VmSnapshotPayload, SnapshotError> {
    let meta_path = dir.join(META);
    let meta_bytes = read_file(&meta_path)?;
    let meta: VmSnapshotMeta =
        serde_json::from_slice(&meta_bytes).map_err(|error| SnapshotError::InvalidManifest {
            path: meta_path.clone(),
            detail: format!("parse meta.json: {error}"),
        })?;
    if meta.magic != BUNDLE_MAGIC {
        return Err(SnapshotError::BadMagic {
            path: Some(meta_path),
            expected: BUNDLE_MAGIC,
        });
    }
    if meta.version != BUNDLE_VERSION {
        return Err(SnapshotError::UnsupportedVersion {
            kind: "vm-bundle",
            found: meta.version,
            supported: BUNDLE_VERSION,
        });
    }
    let gic = read_file(&dir.join(GIC))?;
    let ram = read_file(&dir.join(RAM))?;
    if ram.len() as u64 != meta.ram_size {
        tracing::warn!(
            target: "ternvale::snapshot",
            expected = meta.ram_size,
            found = ram.len(),
            "guest.ram size differs from meta.ram_size"
        );
    }
    let vcpus_json = String::from_utf8(read_file(&dir.join(VCPUS))?).map_err(|error| {
        SnapshotError::InvalidManifest {
            path: dir.join(VCPUS),
            detail: format!("vcpus.json not utf-8: {error}"),
        }
    })?;
    let devices_json = String::from_utf8(read_file(&dir.join(DEVICES))?).map_err(|error| {
        SnapshotError::InvalidManifest {
            path: dir.join(DEVICES),
            detail: format!("devices.json not utf-8: {error}"),
        }
    })?;
    tracing::debug!(
        target: "ternvale::snapshot",
        label = %meta.label,
        vm = %meta.vm_name,
        "vm snapshot bundle loaded"
    );
    Ok(VmSnapshotPayload {
        meta,
        gic,
        ram,
        vcpus_json,
        devices_json,
    })
}

/// List snapshot labels under `vm_root/<vm_name>/`.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(vm_root = %vm_root.display(), vm = vm_name))]
pub fn list_snapshots(vm_root: &Path, vm_name: &str) -> Result<Vec<String>, SnapshotError> {
    let dir = vm_root.join(vm_name);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut labels = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|source| io_err(&dir, source))? {
        let path = entry.map_err(|source| io_err(&dir, source))?.path();
        if path.is_dir() && path.join(META).is_file() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                labels.push(name.to_string());
            }
        }
    }
    labels.sort();
    Ok(labels)
}

/// Remove one bundle directory.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(bundle = %bundle.display()))]
pub fn delete_bundle(bundle: &Path) -> Result<(), SnapshotError> {
    if !bundle.join(META).is_file() {
        return Err(SnapshotError::InvalidManifest {
            path: bundle.to_path_buf(),
            detail: "not a vm snapshot bundle (missing meta.json)".to_string(),
        });
    }
    fs::remove_dir_all(bundle).map_err(|source| io_err(bundle, source))?;
    tracing::info!(target: "ternvale::snapshot", path = %bundle.display(), "vm snapshot deleted");
    Ok(())
}

fn write_bytes(path: PathBuf, data: &[u8]) -> Result<(), SnapshotError> {
    let mut file = File::create(&path).map_err(|source| io_err(&path, source))?;
    file.write_all(data)
        .map_err(|source| io_err(&path, source))?;
    Ok(())
}

fn read_file(path: &Path) -> Result<Vec<u8>, SnapshotError> {
    let mut file = File::open(path).map_err(|source| io_err(path, source))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|source| io_err(path, source))?;
    Ok(buf)
}

fn io_err(path: &Path, source: std::io::Error) -> SnapshotError {
    SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_round_trip() {
        let dir = std::env::temp_dir().join(format!("tvsb-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let meta = new_meta("vm", "snap1", 0x4000_0000, 64 * 1024 * 1024, 8, 2);
        let payload = VmSnapshotPayload {
            meta,
            gic: vec![1, 2, 3, 4, 5, 6, 7, 8],
            ram: vec![0xAA; 1024],
            vcpus_json: r#"[]"#.to_string(),
            devices_json: r#"{}"#.to_string(),
        };
        write_bundle(&dir, &payload).expect("write");
        let back = read_bundle(&dir).expect("read");
        assert_eq!(back.meta.label, "snap1");
        assert_eq!(back.gic, payload.gic);
        assert_eq!(back.ram.len(), 1024);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_unknown_bundle_version() {
        let dir = std::env::temp_dir().join(format!("tvsb-ver-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let meta = new_meta("vm", "x", 0, 0, 0, 1);
        let bad = VmSnapshotMeta {
            version: 99,
            ..meta
        };
        let json = serde_json::to_vec_pretty(&bad).expect("json");
        fs::write(dir.join(META), json).expect("meta");
        fs::write(dir.join(GIC), &[]).expect("gic");
        fs::write(dir.join(RAM), &[]).expect("ram");
        fs::write(dir.join(VCPUS), "[]").expect("vcpus");
        fs::write(dir.join(DEVICES), "{}").expect("devices");
        let err = read_bundle(&dir).expect_err("read");
        assert!(matches!(
            err,
            SnapshotError::UnsupportedVersion {
                kind: "vm-bundle",
                found: 99,
                ..
            }
        ));
        fs::remove_dir_all(&dir).ok();
    }
}
