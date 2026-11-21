//! Full-VM snapshot bundles (guest RAM + device metadata).
//!
//! v1 is a full uncompressed RAM dump. v2 adds zstd, dirty-page incrementals,
//! and a SHA-256 chain. See `book/snapshots.md`.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use ternvale_crypto::{GcmKey, AAD_SNAP_DEV, AAD_SNAP_GIC, AAD_SNAP_RAM, AAD_SNAP_VCPU};

use crate::SnapshotError;

/// Magic in `meta.json` (`TVSB`).
pub const BUNDLE_MAGIC: &str = "TVSB";
/// Current bundle format (zstd + incremental dirty pages).
pub const BUNDLE_VERSION: u32 = 2;
/// Oldest readable bundle version.
pub const BUNDLE_VERSION_MIN: u32 = 1;
/// Uncompressed RAM cache for lazy `mmap` restore.
pub const RAM_CACHE: &str = "guest.ram.lazy";
/// Packed dirty pages (zstd).
pub const DIRTY_FILE: &str = "dirty.bin";

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
    /// `host` or `baseline`. Omitted on bundles written before Step 103.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_profile: Option<String>,
    /// Guest-visible ID / feature registers as hex (`0x…`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub id_registers: BTreeMap<String, String>,
    /// Parent snapshot label for an incremental.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// `"zstd"` when RAM/dirty payloads are compressed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression: Option<String>,
    /// SHA-256 of the logical RAM image (integrity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_sha256: Option<String>,
    /// Dirty page count for incremental bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty_pages: Option<u64>,
}

/// Guest RAM plus GIC blob and JSON sidecars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmSnapshotPayload {
    pub meta: VmSnapshotMeta,
    pub gic: Vec<u8>,
    pub ram: Vec<u8>,
    pub vcpus_json: String,
    pub devices_json: String,
    /// Incremental dirty pages `(gpa, page_bytes)`. Empty for a full dump.
    pub dirty: Vec<(u64, Vec<u8>)>,
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
            "Virtio in-flight requests are not restored; disks use layered .tstack snapshots separately.".to_string(),
        ],
        cpu_profile: None,
        id_registers: BTreeMap::new(),
        parent: None,
        compression: Some("zstd".into()),
        ram_sha256: None,
        dirty_pages: None,
    }
}

/// Write a versioned bundle directory (created if missing).
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(dir = %dir.display()))]
pub fn write_bundle(dir: &Path, payload: &VmSnapshotPayload) -> Result<(), SnapshotError> {
    write_bundle_crypto(dir, payload, None)
}

/// [`write_bundle`], sealing RAM/GIC/JSON with AES-256-GCM when `gcm` is set.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(dir = %dir.display(), encrypted = gcm.is_some()))]
pub fn write_bundle_crypto(
    dir: &Path,
    payload: &VmSnapshotPayload,
    gcm: Option<&GcmKey>,
) -> Result<(), SnapshotError> {
    if payload.meta.magic != BUNDLE_MAGIC
        || !(BUNDLE_VERSION_MIN..=BUNDLE_VERSION).contains(&payload.meta.version)
    {
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
    write_protected(dir.join(GIC), &payload.gic, gcm, AAD_SNAP_GIC)?;
    let ram_store =
        if payload.meta.compression.as_deref() == Some("zstd") && !payload.ram.is_empty() {
            crate::ram::compress(&payload.ram)?
        } else {
            payload.ram.clone()
        };
    write_protected(dir.join(RAM), &ram_store, gcm, AAD_SNAP_RAM)?;
    if !payload.dirty.is_empty() {
        let packed = crate::ram::pack_dirty(
            crate::ram::pack_dirty_page_hint(&payload.dirty),
            &payload.dirty,
        )?;
        let stored = if payload.meta.compression.as_deref() == Some("zstd") {
            crate::ram::compress(&packed)?
        } else {
            packed
        };
        write_protected(dir.join(DIRTY_FILE), &stored, gcm, AAD_SNAP_RAM)?;
    }
    write_protected(
        dir.join(VCPUS),
        payload.vcpus_json.as_bytes(),
        gcm,
        AAD_SNAP_VCPU,
    )?;
    write_protected(
        dir.join(DEVICES),
        payload.devices_json.as_bytes(),
        gcm,
        AAD_SNAP_DEV,
    )?;
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
    read_bundle_crypto(dir, None)
}

/// [`read_bundle`], opening GCM envelopes when `gcm` is set.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(dir = %dir.display(), encrypted = gcm.is_some()))]
pub fn read_bundle_crypto(
    dir: &Path,
    gcm: Option<&GcmKey>,
) -> Result<VmSnapshotPayload, SnapshotError> {
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
    if !(BUNDLE_VERSION_MIN..=BUNDLE_VERSION).contains(&meta.version) {
        return Err(SnapshotError::UnsupportedVersion {
            kind: "vm-bundle",
            found: meta.version,
            supported: BUNDLE_VERSION,
        });
    }
    let gic = read_protected(&dir.join(GIC), gcm, AAD_SNAP_GIC)?;
    let ram_stored = read_protected(&dir.join(RAM), gcm, AAD_SNAP_RAM)?;
    let ram = decode_ram(&meta, &ram_stored)?;
    if meta.ram_policy != "incremental" && ram.len() as u64 != meta.ram_size && meta.ram_size != 0 {
        tracing::warn!(
            target: "ternvale::snapshot",
            expected = meta.ram_size,
            found = ram.len(),
            "guest.ram size differs from meta.ram_size"
        );
    }
    if let Some(expect) = &meta.ram_sha256 {
        if meta.ram_policy != "incremental" {
            let got = crate::ram::sha256_hex(&ram);
            if got != *expect {
                return Err(SnapshotError::InvalidManifest {
                    path: dir.join(RAM),
                    detail: format!("integrity check failed for snapshot `{}`", meta.label),
                });
            }
        }
    }
    let dirty = read_dirty(dir, &meta, gcm)?;
    let vcpus_json = String::from_utf8(read_protected(&dir.join(VCPUS), gcm, AAD_SNAP_VCPU)?)
        .map_err(|error| SnapshotError::InvalidManifest {
            path: dir.join(VCPUS),
            detail: format!("vcpus.json not utf-8: {error}"),
        })?;
    let devices_json = String::from_utf8(read_protected(&dir.join(DEVICES), gcm, AAD_SNAP_DEV)?)
        .map_err(|error| SnapshotError::InvalidManifest {
            path: dir.join(DEVICES),
            detail: format!("devices.json not utf-8: {error}"),
        })?;
    tracing::debug!(
        target: "ternvale::snapshot",
        label = %meta.label,
        vm = %meta.vm_name,
        policy = %meta.ram_policy,
        "vm snapshot bundle loaded"
    );
    Ok(VmSnapshotPayload {
        meta,
        gic,
        ram,
        vcpus_json,
        devices_json,
        dirty,
    })
}

fn decode_ram(meta: &VmSnapshotMeta, stored: &[u8]) -> Result<Vec<u8>, SnapshotError> {
    if stored.is_empty() {
        return Ok(Vec::new());
    }
    if meta.version >= 2 && meta.compression.as_deref() == Some("zstd") {
        let max = usize::try_from(meta.ram_size.saturating_add(64 * 1024)).unwrap_or(usize::MAX);
        crate::ram::decompress(stored, max)
    } else {
        Ok(stored.to_vec())
    }
}

fn read_dirty(
    dir: &Path,
    meta: &VmSnapshotMeta,
    gcm: Option<&GcmKey>,
) -> Result<Vec<(u64, Vec<u8>)>, SnapshotError> {
    let path = dir.join(DIRTY_FILE);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let stored = read_protected(&path, gcm, AAD_SNAP_RAM)?;
    let raw = if meta.compression.as_deref() == Some("zstd") {
        crate::ram::decompress(&stored, 64 * 1024 * 1024)?
    } else {
        stored
    };
    crate::ram::unpack_dirty(&raw)
}

/// Write uncompressed RAM to `dir/guest.ram.lazy` for a private mmap restore.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(dir = %dir.display()))]
pub fn materialize_ram(dir: &Path, ram: &[u8]) -> Result<PathBuf, SnapshotError> {
    let path = dir.join(RAM_CACHE);
    fs::write(&path, ram).map_err(|source| io_err(&path, source))?;
    tracing::info!(
        target: "ternvale::snapshot",
        path = %path.display(),
        bytes = ram.len(),
        "materialized lazy RAM cache"
    );
    Ok(path)
}

/// Replay a snapshot chain into `guest.ram.lazy` (base + dirty pages).
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(dir = %dir.display()))]
pub fn collapse_chain(
    dir: &Path,
    gcm: Option<&GcmKey>,
    ram_base: u64,
    ram_size: u64,
) -> Result<PathBuf, SnapshotError> {
    use std::os::unix::fs::FileExt;
    let meta = read_bundle_crypto(dir, gcm)?.meta;
    let root = dir.parent().ok_or_else(|| SnapshotError::InvalidRequest {
        detail: format!("snapshot `{}` has no parent directory", meta.label),
    })?;
    let chain = crate::chain::SnapshotChain::load(root)?;
    let lineage = chain.lineage(&meta.label)?;
    let cache = dir.join(RAM_CACHE);
    let mut wrote = false;
    for node in lineage {
        let payload = read_bundle_crypto(&root.join(&node.label), gcm).map_err(|error| {
            SnapshotError::InvalidManifest {
                path: root.join(&node.label),
                detail: format!("corrupt snapshot chain link `{}`: {error}", node.label),
            }
        })?;
        if payload.meta.ram_policy != "incremental" {
            fs::write(&cache, &payload.ram).map_err(|source| io_err(&cache, source))?;
            wrote = true;
        } else {
            if !wrote {
                return Err(SnapshotError::InvalidManifest {
                    path: root.join(&node.label),
                    detail: format!(
                        "corrupt snapshot chain link `{}`: incremental without a full base",
                        node.label
                    ),
                });
            }
            let file = fs::OpenOptions::new()
                .write(true)
                .open(&cache)
                .map_err(|source| io_err(&cache, source))?;
            for (gpa, page) in payload.dirty {
                let off = gpa.saturating_sub(ram_base);
                if off.saturating_add(page.len() as u64) > ram_size {
                    return Err(SnapshotError::InvalidManifest {
                        path: root.join(&node.label),
                        detail: format!("dirty page {gpa:#x} outside RAM in `{}`", node.label),
                    });
                }
                file.write_all_at(&page, off)
                    .map_err(|source| io_err(&cache, source))?;
            }
        }
    }
    if !wrote {
        return Err(SnapshotError::InvalidManifest {
            path: dir.to_path_buf(),
            detail: format!("missing snapshot chain link `{}`", meta.label),
        });
    }
    Ok(cache)
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

/// True when `guest.ram` is a GCM envelope (clone can copy without the DEK).
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(dir = %dir.display()))]
pub fn bundle_encrypted(dir: &Path) -> bool {
    let Ok(bytes) = read_file(&dir.join(RAM)) else {
        return false;
    };
    ternvale_crypto::is_envelope(&bytes)
}

fn write_protected(
    path: PathBuf,
    data: &[u8],
    gcm: Option<&GcmKey>,
    aad: &[u8],
) -> Result<(), SnapshotError> {
    if let Some(gcm) = gcm {
        ternvale_crypto::write_gcm_file(gcm, &path, data, aad).map_err(|error| {
            SnapshotError::InvalidRequest {
                detail: error.to_string(),
            }
        })
    } else {
        write_bytes(path, data)
    }
}

fn read_protected(path: &Path, gcm: Option<&GcmKey>, aad: &[u8]) -> Result<Vec<u8>, SnapshotError> {
    ternvale_crypto::read_gcm_file(gcm, path, aad).map_err(|error| SnapshotError::InvalidRequest {
        detail: error.to_string(),
    })
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
            dirty: Vec::new(),
        };
        write_bundle(&dir, &payload).expect("write");
        let back = read_bundle(&dir).expect("read");
        assert_eq!(back.meta.label, "snap1");
        assert_eq!(back.gic, payload.gic);
        assert_eq!(back.ram.len(), 1024);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gcm_bundle_hides_ram_and_rejects_tamper() {
        let dir = std::env::temp_dir().join(format!("tvsb-gcm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let gcm = GcmKey::from_key(&ternvale_crypto::Secret::new(vec![4u8; 32])).expect("gcm");
        let meta = new_meta("vm", "enc", 0x4000_0000, 1024, 8, 1);
        let payload = VmSnapshotPayload {
            meta,
            gic: vec![9; 8],
            ram: b"TERNVALE-PLAINTEXT-MARKER".to_vec(),
            vcpus_json: "[]".into(),
            devices_json: "{}".into(),
            dirty: Vec::new(),
        };
        write_bundle_crypto(&dir, &payload, Some(&gcm)).expect("write");
        let raw = fs::read(dir.join(RAM)).expect("ram");
        assert!(!raw
            .windows(b"TERNVALE-PLAINTEXT-MARKER".len())
            .any(|w| w == b"TERNVALE-PLAINTEXT-MARKER"));
        assert!(bundle_encrypted(&dir));
        let back = read_bundle_crypto(&dir, Some(&gcm)).expect("read");
        assert_eq!(back.ram, payload.ram);
        let mut tampered = raw;
        let mid = tampered.len() / 2;
        tampered[mid] ^= 1;
        fs::write(dir.join(RAM), &tampered).expect("tamper");
        let err = read_bundle_crypto(&dir, Some(&gcm)).expect_err("auth");
        assert!(
            err.to_string().contains("authentication")
                || err.to_string().contains("wrong key")
                || err.to_string().contains("modified")
                || err.to_string().contains("invalid"),
            "{err}"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn old_meta_without_cpu_fields_still_loads() {
        let json = r#"{
            "magic": "TVSB",
            "version": 1,
            "vm_name": "vm",
            "label": "old",
            "created_unix_ms": 0,
            "ram_base": 0,
            "ram_size": 0,
            "ram_policy": "full",
            "gic_bytes": 0,
            "vcpu_count": 1,
            "limitations": []
        }"#;
        let meta: VmSnapshotMeta = serde_json::from_str(json).expect("old meta");
        assert!(meta.cpu_profile.is_none());
        assert!(meta.id_registers.is_empty());
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

    #[test]
    fn incremental_chain_and_corrupt_link() {
        use crate::chain::{ChainNode, SnapshotChain};
        let root = std::env::temp_dir().join(format!("tvsb-chain-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let page = 16 * 1024;
        let mut ram = vec![0u8; page];
        ram[0] = 1;
        let mut meta = new_meta("vm", "a", 0, page as u64, 0, 1);
        meta.ram_sha256 = Some(crate::ram::sha256_hex(&ram));
        write_bundle(
            &root.join("a"),
            &VmSnapshotPayload {
                meta,
                gic: vec![],
                ram: ram.clone(),
                vcpus_json: "[]".into(),
                devices_json: "{}".into(),
                dirty: vec![],
            },
        )
        .expect("base");
        ram[0] = 9;
        let dirty = vec![(0u64, ram.clone())];
        let mut inc = new_meta("vm", "b", 0, page as u64, 0, 1);
        inc.ram_policy = "incremental".into();
        inc.parent = Some("a".into());
        write_bundle(
            &root.join("b"),
            &VmSnapshotPayload {
                meta: inc,
                gic: vec![],
                ram: vec![],
                vcpus_json: "[]".into(),
                devices_json: "{}".into(),
                dirty,
            },
        )
        .expect("inc");
        let mut chain = SnapshotChain::empty();
        chain.upsert(ChainNode {
            label: "a".into(),
            parent: None,
            ram_sha256: "x".into(),
            ram_policy: "full".into(),
        });
        chain.upsert(ChainNode {
            label: "b".into(),
            parent: Some("a".into()),
            ram_sha256: "y".into(),
            ram_policy: "incremental".into(),
        });
        chain.save(&root).expect("chain");
        let lazy = collapse_chain(&root.join("b"), None, 0, page as u64).expect("collapse");
        let out = fs::read(&lazy).expect("lazy");
        assert_eq!(out[0], 9);
        let err = SnapshotChain::load(&root)
            .expect("load")
            .lineage("missing")
            .expect_err("link");
        assert!(err.to_string().contains("`missing`"), "{err}");
        fs::remove_dir_all(&root).ok();
    }
}
