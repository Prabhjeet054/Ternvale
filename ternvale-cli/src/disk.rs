//! `ternvale create-disk`: sparse raw images for virtio-blk.

use std::path::Path;

use anyhow::{bail, Context, Result};

/// virtio-blk sector size; image sizes must be a whole number of sectors.
pub const SECTOR: u64 = 512;

/// Parse `1048576`, `512K`, `64M`, `4G`, `1T` (also `KiB`/`MiB`/`GiB`/`TiB`,
/// any case). Powers of 1024. Must be a positive multiple of 512.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(text))]
pub fn parse_size(text: &str) -> Result<u64> {
    let trimmed = text.trim();
    let split = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (digits, unit) = trimmed.split_at(split);
    if digits.is_empty() {
        bail!("size {text:?} must start with a number");
    }
    let number: u64 = digits
        .parse()
        .with_context(|| format!("size {text:?} is not a number"))?;
    let shift = match unit.to_ascii_uppercase().as_str() {
        "" | "B" => 0,
        "K" | "KB" | "KIB" => 10,
        "M" | "MB" | "MIB" => 20,
        "G" | "GB" | "GIB" => 30,
        "T" | "TB" | "TIB" => 40,
        other => bail!("size {text:?} has an unknown unit {other:?} (use K, M, G, or T)"),
    };
    let bytes = number
        .checked_mul(1u64 << shift)
        .with_context(|| format!("size {text:?} overflows 64 bits"))?;
    if bytes == 0 {
        bail!("size {text:?} must be larger than zero");
    }
    if bytes % SECTOR != 0 {
        bail!("size {text:?} ({bytes} bytes) is not a multiple of {SECTOR}");
    }
    Ok(bytes)
}

/// Create `path` as a sparse file of `bytes`. Refuses to overwrite.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display(), bytes))]
pub fn create_disk(path: &Path, bytes: u64) -> Result<()> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| {
            format!(
                "create disk image {} (it must not exist yet)",
                path.display()
            )
        })?;
    file.set_len(bytes)
        .with_context(|| format!("size disk image {} to {bytes} bytes", path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync disk image {}", path.display()))?;
    tracing::info!(target: "ternvale::cli", path = %path.display(), bytes, "created sparse disk image");
    Ok(())
}

/// `4294967296` → `4 GiB`; sizes that are not whole units stay in bytes.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(bytes))]
pub fn human_size(bytes: u64) -> String {
    for (shift, unit) in [(40, "TiB"), (30, "GiB"), (20, "MiB"), (10, "KiB")] {
        if bytes >= 1 << shift && bytes % (1 << shift) == 0 {
            return format!("{} {unit}", bytes >> shift);
        }
    }
    format!("{bytes} bytes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes_with_binary_units() {
        assert_eq!(parse_size("1048576").expect("bytes"), 1 << 20);
        assert_eq!(parse_size("512K").expect("k"), 512 << 10);
        assert_eq!(parse_size("64m").expect("m"), 64 << 20);
        assert_eq!(parse_size("4GiB").expect("g"), 4 << 30);
        assert_eq!(parse_size(" 1T ").expect("t"), 1 << 40);
        for bad in ["", "G", "0", "100", "1X", "-1G", "99999999999T"] {
            assert!(parse_size(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn creates_a_sparse_image_and_refuses_to_overwrite() {
        let dir = std::env::temp_dir().join(format!("ternvale-cli-disk-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("clear leftovers");
        }
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("disk.img");
        create_disk(&path, 8 << 30).expect("create");
        let meta = std::fs::metadata(&path).expect("meta");
        assert_eq!(meta.len(), 8 << 30);
        {
            use std::os::unix::fs::MetadataExt;
            assert!(
                meta.blocks() * 512 < 1 << 20,
                "image is not sparse: {} blocks",
                meta.blocks()
            );
        }
        let error = create_disk(&path, 1 << 20).expect_err("overwrite");
        assert!(format!("{error:#}").contains("must not exist"), "{error:#}");
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn prints_whole_units() {
        assert_eq!(human_size(4 << 30), "4 GiB");
        assert_eq!(human_size(1536 << 20), "1536 MiB");
        assert_eq!(human_size(1000), "1000 bytes");
    }
}
