//! Portable `.tvm` VM bundle: a directory with `MANIFEST.json`.
//!
//! Magic `TERNVALE-TVM` and integer `version`. Unknown versions are rejected.
//! Every payload path is relative, has no `..`, and is listed with SHA-256.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

/// On-disk magic string.
pub const MAGIC: &str = "TERNVALE-TVM";
/// Current manifest layout.
pub const VERSION: u32 = 1;
/// Manifest file name inside the bundle directory.
pub const MANIFEST: &str = "MANIFEST.json";

/// How a payload is stored in the bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    /// Plain file (sparse-preserving copy).
    Raw,
    /// zstd frame of the logical bytes.
    Zstd,
}

/// Role of one listed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileKind {
    /// Rewritten `config.toml`.
    Config,
    /// Raw disk image or stack base.
    Disk,
    /// `.tstack` or `.tdelta` layer.
    DiskLayer,
    /// UEFI variable store.
    Nvram,
    /// swtpm state (`*.swtpm`).
    Tpm,
    /// Kernel, initrd, or firmware copied into `boot/`.
    Boot,
}

/// One file inside the bundle (paths relative to the `.tvm` directory).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TvmFile {
    /// POSIX-style relative path.
    pub path: String,
    /// SHA-256 of the stored bytes (lowercase hex).
    pub sha256: String,
    /// Stored size in bytes.
    pub bytes: u64,
    /// SHA-256 of the logical (decompressed, holes-as-zeros) bytes.
    pub logical_sha256: String,
    /// Logical size in bytes.
    pub logical_bytes: u64,
    /// Storage encoding.
    pub encoding: Encoding,
    /// What the file is for.
    pub kind: FileKind,
}

/// Versioned bundle index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TvmManifest {
    /// [`MAGIC`].
    pub magic: String,
    /// [`VERSION`].
    pub version: u32,
    /// Source VM name at export.
    pub source_vm: String,
    /// RFC 3339 timestamp.
    pub created: String,
    /// True when at least one payload used zstd.
    pub zstd: bool,
    /// Every payload except this manifest.
    pub files: Vec<TvmFile>,
}

impl TvmManifest {
    /// New v1 document.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = source_vm))]
    pub fn new(source_vm: String, files: Vec<TvmFile>) -> Self {
        let zstd = files.iter().any(|f| f.encoding == Encoding::Zstd);
        Self {
            magic: MAGIC.to_string(),
            version: VERSION,
            source_vm,
            created: chrono::Utc::now().to_rfc3339(),
            zstd,
            files,
        }
    }

    /// Reject unknown magic/version and unsafe paths rather than guessing.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn parse(json: &str) -> Result<Self, String> {
        let parsed: Self = serde_json::from_str(json).map_err(|error| error.to_string())?;
        if parsed.magic != MAGIC {
            tracing::warn!(
                target: "ternvale::cli",
                magic = %parsed.magic,
                "tvm manifest magic rejected"
            );
            return Err(format!(
                "tvm manifest magic {:?} is not {MAGIC:?}",
                parsed.magic
            ));
        }
        if parsed.version != VERSION {
            tracing::warn!(
                target: "ternvale::cli",
                version = parsed.version,
                "tvm manifest version rejected"
            );
            return Err(format!(
                "tvm manifest version {} is not supported (this build reads {VERSION})",
                parsed.version
            ));
        }
        for file in &parsed.files {
            check_rel(&file.path)?;
            if file.sha256.len() != 64 || file.logical_sha256.len() != 64 {
                return Err(format!("tvm file {} has a truncated SHA-256", file.path));
            }
        }
        Ok(parsed)
    }
}

/// Relative bundle path: no absolute, no `..`, no NUL.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path))]
pub fn check_rel(path: &str) -> Result<(), String> {
    if path.is_empty() || path.contains('\0') || path.starts_with('/') {
        return Err(format!("tvm path {path:?} must be a relative file path"));
    }
    let parsed = Path::new(path);
    for component in parsed.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => return Err(format!("tvm path {path:?} must not contain '..' or a root")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_bad_magic_version_and_dotdot() {
        let good = TvmManifest::new(
            "lab".into(),
            vec![TvmFile {
                path: "config.toml".into(),
                sha256: "a".repeat(64),
                bytes: 1,
                logical_sha256: "b".repeat(64),
                logical_bytes: 1,
                encoding: Encoding::Raw,
                kind: FileKind::Config,
            }],
        );
        let json = serde_json::to_string(&good).expect("enc");
        TvmManifest::parse(&json).expect("ok");
        let bad_magic = json.replace(MAGIC, "NOPE");
        assert!(TvmManifest::parse(&bad_magic)
            .unwrap_err()
            .contains("magic"));
        let mut v2 = good.clone();
        v2.version = 99;
        let err = TvmManifest::parse(&serde_json::to_string(&v2).expect("v2")).unwrap_err();
        assert!(err.contains("version 99"), "{err}");
        assert!(check_rel("../etc/passwd").is_err());
        assert!(check_rel("/abs").is_err());
        assert!(check_rel("disks/disk0.img").is_ok());
    }
}
