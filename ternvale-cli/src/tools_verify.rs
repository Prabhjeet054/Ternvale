//! Checksum a mounted tools ISO (or staging tree) against `MANIFEST.json`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::tools_manifest::ToolsManifest;

/// Check every `files[]` entry under `root`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(root = %root.display(), files = manifest.files.len()))]
pub fn verify_tree(manifest: &ToolsManifest, root: &Path) -> Result<()> {
    for file in &manifest.files {
        let path = match find_relative(root, &file.path) {
            Some(path) => path,
            None => bail!(
                "manifest lists {} but it is missing under {}",
                file.path,
                root.display()
            ),
        };
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        if bytes.len() as u64 != file.bytes {
            bail!(
                "{} size {} != manifest {}",
                file.path,
                bytes.len(),
                file.bytes
            );
        }
        let digest = Sha256::digest(&bytes);
        let sha256: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        if sha256 != file.sha256 {
            bail!("{} sha256 {sha256} != manifest {}", file.path, file.sha256);
        }
        tracing::debug!(
            target: "ternvale::cli",
            path = %file.path,
            bytes = file.bytes,
            "tools file checksum ok"
        );
    }
    tracing::info!(
        target: "ternvale::cli",
        root = %root.display(),
        files = manifest.files.len(),
        "tools ISO file checksums match the manifest"
    );
    Ok(())
}

fn find_relative(root: &Path, rel: &str) -> Option<PathBuf> {
    let direct = root.join(rel);
    if direct.is_file() {
        return Some(direct);
    }
    let want = rel.replace('\\', "/").to_ascii_lowercase();
    walk_find(root, root, &want)
}

fn walk_find(root: &Path, dir: &Path, want: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = walk_find(root, &path, want) {
                return Some(found);
            }
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        if rel == want {
            return Some(path);
        }
    }
    None
}
