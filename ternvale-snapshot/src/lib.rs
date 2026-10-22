//! Layered copy-on-write disk images for virtio-blk.
//!
//! See `docs/SNAPSHOT.md` for the on-disk layout and block-size tradeoffs.

mod delta;
mod format;
mod manifest;
mod stack;

pub use delta::DeltaLayer;
pub use format::{
    bitmap_bytes_for_blocks, bitmap_rank_before, bitmap_set, bitmap_test, DeltaHeader,
    DEFAULT_BLOCK_SIZE, DELTA_HEADER_LEN, DELTA_MAGIC, DELTA_VERSION, STACK_MAGIC, STACK_VERSION,
};
pub use manifest::StackManifest;
pub use stack::ImageStack;

/// Stack / delta errors.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// I/O on a host path failed.
    #[error("snapshot I/O on {path}: {source}")]
    Io {
        /// File involved.
        path: std::path::PathBuf,
        /// OS error.
        source: std::io::Error,
    },
    /// Magic bytes do not match.
    #[error("snapshot bad magic (expected {expected}){}", path.as_ref().map(|p| format!(" in {}", p.display())).unwrap_or_default())]
    BadMagic {
        /// Optional path for context.
        path: Option<std::path::PathBuf>,
        /// Expected magic label.
        expected: &'static str,
    },
    /// File version is not supported.
    #[error("unsupported {kind} snapshot version {found} (supported: {supported})")]
    UnsupportedVersion {
        /// `"stack"` or `"delta"`.
        kind: &'static str,
        /// Version read from disk.
        found: u32,
        /// Highest version this build supports.
        supported: u32,
    },
    /// Truncated file.
    #[error("truncated snapshot: {what}")]
    Truncated {
        /// What was being parsed.
        what: &'static str,
    },
    /// Manifest or request invalid.
    #[error("invalid snapshot{path}: {detail}", path = path.display())]
    InvalidManifest {
        /// Related path.
        path: std::path::PathBuf,
        /// Detail.
        detail: String,
    },
    /// Invalid API use.
    #[error("invalid snapshot request: {detail}")]
    InvalidRequest {
        /// Detail.
        detail: String,
    },
    /// Layer is not writable.
    #[error("snapshot layer is read-only: {path}")]
    ReadOnly {
        /// Delta path.
        path: std::path::PathBuf,
    },
}

/// Whether `path` is a stack manifest (`.tstack` or `TVST` magic).
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(path = %path.display()))]
pub fn is_stack_manifest(path: &std::path::Path) -> bool {
    if path.extension().is_some_and(|ext| ext == "tstack") {
        return true;
    }
    let mut magic = [0u8; 4];
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    magic == STACK_MAGIC
}

#[cfg(test)]
#[path = "layer_tests.rs"]
mod layer_tests;

#[cfg(test)]
mod tests {
    use std::os::unix::fs::FileExt;

    use super::*;

    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-snapshot");
    }

    #[test]
    fn cow_stack_read_write() {
        let dir =
            std::env::temp_dir().join(format!("ternvale-snap-{}-{}", std::process::id(), "cow"));
        std::fs::create_dir_all(&dir).expect("dir");
        let base = dir.join("base.img");
        let delta = dir.join("snap.tdelta");
        let stack_path = dir.join("disk.tstack");
        {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&base)
                .expect("base");
            file.set_len(256 * 1024).expect("len");
            let pattern = [0xABu8; 512];
            for sector in 0..512usize {
                file.write_all_at(&pattern, sector as u64 * 512)
                    .expect("seed");
            }
        }
        DeltaLayer::create(&delta, 256 * 1024).expect("delta");
        let manifest = StackManifest {
            base: std::path::PathBuf::from("base.img"),
            deltas: vec![std::path::PathBuf::from("snap.tdelta")],
        };
        manifest.save(&stack_path).expect("manifest");

        let mut image = ImageStack::open(&stack_path, false).expect("open");
        let mut buf = [0u8; 512];
        image.read_at(&mut buf, 0).expect("read base");
        assert_eq!(buf[0], 0xAB);

        let write = [0xCDu8; 512];
        image.write_at(&write, 64 * 1024).expect("write cow");
        image.read_at(&mut buf, 64 * 1024).expect("read cow");
        assert_eq!(buf[0], 0xCD);
        image.read_at(&mut buf, 0).expect("read unchanged");
        assert_eq!(buf[0], 0xAB);

        let ro = ImageStack::open(&stack_path, true).expect("ro");
        ro.read_at(&mut buf, 64 * 1024).expect("ro read");
        assert_eq!(buf[0], 0xCD);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_unknown_delta_version() {
        let dir = std::env::temp_dir().join(format!("ternvale-snap-ver-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("bad.tdelta");
        let mut bytes = [0u8; 64];
        bytes[0..4].copy_from_slice(&DELTA_MAGIC);
        bytes[4..8].copy_from_slice(&99u32.to_le_bytes());
        std::fs::write(&path, bytes).expect("write");
        let err = DeltaLayer::open(&path, false).expect_err("open");
        assert!(matches!(
            err,
            SnapshotError::UnsupportedVersion {
                kind: "delta",
                found: 99,
                ..
            }
        ));
        std::fs::remove_dir_all(&dir).ok();
    }
}
