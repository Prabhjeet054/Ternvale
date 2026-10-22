//! Linked-clone disk stacks: shared base + fresh writable delta.

use std::path::{Path, PathBuf};

use crate::base_ref;
use crate::delta::DeltaLayer;
use crate::manifest::StackManifest;
use crate::{is_stack_manifest, SnapshotError};

/// Resolve a VM disk path to the shared base image (ignoring existing deltas).
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(path = %disk_path.display()))]
pub fn shared_base_path(disk_path: &Path) -> Result<PathBuf, SnapshotError> {
    if is_stack_manifest(disk_path) {
        let manifest = StackManifest::load(disk_path)?;
        Ok(manifest.base)
    } else {
        Ok(disk_path.to_path_buf())
    }
}

/// Base length rounded down to the COW block size.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(base = %base.display()))]
pub fn base_virtual_size(base: &Path) -> Result<u64, SnapshotError> {
    let len = std::fs::metadata(base)
        .map_err(|source| SnapshotError::Io {
            path: base.to_path_buf(),
            source,
        })?
        .len();
    let block = u64::from(crate::format::DEFAULT_BLOCK_SIZE);
    Ok(len - (len % block))
}

/// Create a new `.tstack` under `dest_dir` sharing `source_disk`'s base with a fresh delta.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::snapshot",
    skip_all,
    fields(source = %source_disk.display(), dest = %dest_dir.display(), holder)
)]
pub fn create_linked_stack(
    source_disk: &Path,
    dest_dir: &Path,
    stack_name: &str,
    delta_name: &str,
    holder: &str,
) -> Result<PathBuf, SnapshotError> {
    let base = shared_base_path(source_disk)?;
    if !base.is_file() {
        return Err(SnapshotError::InvalidRequest {
            detail: format!("shared base {} does not exist", base.display()),
        });
    }
    base_ref::acquire(&base, holder)?;
    let size = base_virtual_size(&base)?;
    std::fs::create_dir_all(dest_dir).map_err(|source| SnapshotError::Io {
        path: dest_dir.to_path_buf(),
        source,
    })?;
    let delta_path = dest_dir.join(delta_name);
    DeltaLayer::create(&delta_path, size)?;
    let stack_path = dest_dir.join(stack_name);
    let manifest = StackManifest {
        base: base.clone(),
        deltas: vec![delta_path],
    };
    manifest.save(&stack_path)?;
    tracing::info!(
        target: "ternvale::snapshot",
        stack = %stack_path.display(),
        base = %base.display(),
        holder,
        virtual_size = size,
        "linked clone disk stack created"
    );
    Ok(stack_path)
}

/// Release this holder's refcount on the base behind `disk_path` (if any).
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(holder, disk = %disk_path.display()))]
pub fn release_disk_base(disk_path: &Path, holder: &str) -> Result<(), SnapshotError> {
    let base = shared_base_path(disk_path)?;
    if base_ref::count(&base)? > 0 {
        base_ref::release(&base, holder)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linked_stack_uses_shared_base() {
        let dir = std::env::temp_dir().join(format!("tclone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        unsafe { std::env::set_var("TERNVALE_REFS_DIR", dir.join("refs")) };
        let base = dir.join("base.img");
        {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .open(&base)
                .expect("base");
            f.set_len(256 * 1024).expect("len");
        }
        let source_stack = dir.join("source.tstack");
        let manifest = StackManifest {
            base: base.clone(),
            deltas: vec![],
        };
        manifest.save(&source_stack).expect("manifest");
        let clone_dir = dir.join("clone");
        let stack = create_linked_stack(
            &source_stack,
            &clone_dir,
            "disk.tstack",
            "cow.tdelta",
            "clone-a",
        )
        .expect("clone");
        let loaded = StackManifest::load(&stack).expect("load");
        assert_eq!(loaded.base, base.canonicalize().expect("canon"));
        assert_eq!(loaded.deltas.len(), 1);
        assert_eq!(base_ref::count(&base).expect("cnt"), 1);
        release_disk_base(&stack, "clone-a").expect("rel");
        assert_eq!(base_ref::count(&base).expect("cnt"), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn triple_clone_refcount_and_shared_base_outside_stack_dir() {
        let dir = std::env::temp_dir().join(format!("tclone3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        unsafe { std::env::set_var("TERNVALE_REFS_DIR", dir.join("refs")) };
        let base = dir.join("shared-base.img");
        {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .open(&base)
                .expect("base");
            f.set_len(256 * 1024).expect("len");
        }
        let source_stack = dir.join("source").join("disk0.tstack");
        std::fs::create_dir_all(source_stack.parent().expect("parent")).expect("mkdir");
        StackManifest {
            base: base.clone(),
            deltas: vec![],
        }
        .save(&source_stack)
        .expect("source manifest");
        let mut stacks = Vec::new();
        for name in ["clone-a", "clone-b", "clone-c"] {
            let clone_dir = dir.join(name).join("disks");
            let stack = create_linked_stack(
                &source_stack,
                &clone_dir,
                "disk0.tstack",
                "disk0.tdelta",
                name,
            )
            .expect("linked stack");
            stacks.push((name, stack));
        }
        assert_eq!(base_ref::count(&base).expect("cnt"), 3);
        for (name, stack) in &stacks {
            let loaded = StackManifest::load(stack).expect("load");
            assert_eq!(
                loaded.base.canonicalize().expect("canon"),
                base.canonicalize().expect("canon")
            );
            release_disk_base(stack, name).expect("release");
        }
        assert_eq!(base_ref::count(&base).expect("cnt"), 0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
