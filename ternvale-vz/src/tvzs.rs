//! Versioned wrapper around Apple's `.vzvmsave` blob (`TVZS` + version).

use std::path::Path;

use crate::error::VzError;

/// On-disk magic (`TVZS` = Ternvale VZ State).
pub const MAGIC: [u8; 4] = *b"TVZS";
/// Current layout: 16-byte header then the Apple save file.
pub const VERSION: u32 = 1;
const HEADER: usize = 16;

/// Wrap Apple's hardware-encrypted save file in a versioned header.
#[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all, fields(src = %src.display(), dest = %dest.display()))]
pub fn wrap(src: &Path, dest: &Path) -> Result<(), VzError> {
    let apple = std::fs::read(src).map_err(|error| VzError::Io {
        op: "tvzs_read_apple",
        detail: format!("{}: {error}", src.display()),
    })?;
    let mut out = Vec::with_capacity(HEADER + apple.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&apple);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|error| VzError::Io {
            op: "tvzs_mkdir",
            detail: error.to_string(),
        })?;
    }
    std::fs::write(dest, &out).map_err(|error| VzError::Io {
        op: "tvzs_write",
        detail: format!("{}: {error}", dest.display()),
    })?;
    tracing::info!(
        target: "ternvale::vz",
        dest = %dest.display(),
        apple_bytes = apple.len(),
        "wrote vz machine-state bundle"
    );
    Ok(())
}

/// Strip the TVZS header (or pass through a raw Apple file) into `dest`.
#[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all, fields(src = %src.display(), dest = %dest.display()))]
pub fn unwrap_to(src: &Path, dest: &Path) -> Result<(), VzError> {
    let bytes = std::fs::read(src).map_err(|error| VzError::Io {
        op: "tvzs_read",
        detail: format!("{}: {error}", src.display()),
    })?;
    let apple = apple_blob(&bytes)?;
    std::fs::write(dest, apple).map_err(|error| VzError::Io {
        op: "tvzs_unwrap",
        detail: format!("{}: {error}", dest.display()),
    })?;
    Ok(())
}

fn apple_blob(bytes: &[u8]) -> Result<&[u8], VzError> {
    if bytes.len() >= HEADER && bytes[0..4] == MAGIC {
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap_or([0; 4]));
        if version != VERSION {
            return Err(VzError::Host {
                op: "tvzs",
                detail: format!(
                    "unsupported vz save version {version} (this build reads {VERSION})"
                ),
            });
        }
        return Ok(&bytes[HEADER..]);
    }
    Err(VzError::Host {
        op: "tvzs",
        detail: "not a Ternvale VZ save (expected TVZS magic); the file cannot be restored".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_unwrap_round_trip() {
        let dir = std::env::temp_dir().join(format!("tvzs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let apple = dir.join("apple.vzvmsave");
        let wrapped = dir.join("state.tvzs");
        let out = dir.join("out.vzvmsave");
        std::fs::write(&apple, b"apple-blob").expect("apple");
        wrap(&apple, &wrapped).expect("wrap");
        let raw = std::fs::read(&wrapped).expect("read");
        assert_eq!(&raw[0..4], b"TVZS");
        unwrap_to(&wrapped, &out).expect("unwrap");
        assert_eq!(std::fs::read(&out).expect("out"), b"apple-blob");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_version_is_rejected() {
        let mut bytes = Vec::from(*b"TVZS");
        bytes.extend_from_slice(&99u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 8]);
        bytes.extend_from_slice(b"x");
        let err = apple_blob(&bytes).unwrap_err().to_string();
        assert!(err.contains("version 99"), "{err}");
    }
}
