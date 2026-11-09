//! Versioned security-scoped bookmark store (`TVBK`).

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use ternvale_hv::{
    create_security_scoped_bookmark, resolve_security_scoped_bookmark, ScopedAccess,
};

use super::error::SandboxError;
use super::grants::Grant;

/// File magic.
pub const MAGIC: &[u8; 4] = b"TVBK";
/// Current store version. Unknown versions are rejected.
pub const VERSION: u32 = 1;

/// Persist bookmarks for `grants` at `path`.
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all, fields(path = %path.display(), n = grants.len()))]
pub fn write_store(path: &Path, grants: &[Grant]) -> Result<(), SandboxError> {
    let mut body = Vec::new();
    let count = u32::try_from(grants.len()).unwrap_or(0);
    body.extend_from_slice(&count.to_le_bytes());
    for grant in grants {
        let blob = match create_security_scoped_bookmark(&grant.path) {
            Ok(blob) => blob,
            Err(error) => {
                tracing::warn!(
                    target: "ternvale::sandbox",
                    path = %grant.path.display(),
                    error = %error,
                    "security-scoped bookmark not created; Seatbelt path grant still applies"
                );
                Vec::new()
            }
        };
        put_path(&mut body, &grant.path);
        body.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        body.extend_from_slice(&blob);
    }
    let mut file = File::create(path).map_err(|source| SandboxError::Io {
        op: "create bookmark store",
        source,
    })?;
    file.write_all(MAGIC).map_err(|source| SandboxError::Io {
        op: "write bookmark magic",
        source,
    })?;
    file.write_all(&VERSION.to_le_bytes())
        .map_err(|source| SandboxError::Io {
            op: "write bookmark version",
            source,
        })?;
    file.write_all(&body).map_err(|source| SandboxError::Io {
        op: "write bookmark body",
        source,
    })?;
    tracing::info!(
        target: "ternvale::sandbox",
        path = %path.display(),
        entries = grants.len(),
        "wrote security-scoped bookmark store"
    );
    Ok(())
}

/// Load `path` and start accessing each scoped resource.
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all, fields(path = %path.display()))]
pub fn start_access(path: &Path) -> Result<Vec<ScopedAccess>, SandboxError> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|mut f| f.read_to_end(&mut bytes))
        .map_err(|source| SandboxError::Io {
            op: "read bookmark store",
            source,
        })?;
    let entries = parse_store(&bytes).map_err(|reason| SandboxError::Bookmarks {
        path: path.to_path_buf(),
        reason,
    })?;
    let mut held = Vec::new();
    for (_host, blob) in entries {
        if blob.is_empty() {
            continue;
        }
        match resolve_security_scoped_bookmark(&blob) {
            Ok(access) => held.push(access),
            Err(error) => {
                tracing::warn!(
                    target: "ternvale::sandbox",
                    error = %error,
                    "could not start security-scoped access; Seatbelt path grant still applies"
                );
            }
        }
    }
    tracing::info!(
        target: "ternvale::sandbox",
        path = %path.display(),
        live = held.len(),
        "started security-scoped bookmark access"
    );
    Ok(held)
}

fn put_path(body: &mut Vec<u8>, path: &Path) {
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    body.extend_from_slice(bytes);
}

/// Parse a store into `(path, bookmark blob)` pairs. Rejects unknown versions.
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all)]
pub fn parse_store(bytes: &[u8]) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    if bytes.len() < 8 {
        return Err("truncated header".into());
    }
    if &bytes[..4] != MAGIC {
        return Err("bad magic (want TVBK)".into());
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap_or([0; 4]));
    if version != VERSION {
        return Err(format!(
            "unsupported version {version} (this build knows {VERSION})"
        ));
    }
    let mut rest = &bytes[8..];
    if rest.len() < 4 {
        return Err("truncated count".into());
    }
    let count = u32::from_le_bytes(rest[..4].try_into().unwrap_or([0; 4])) as usize;
    rest = &rest[4..];
    if count > 4096 {
        return Err("too many bookmark entries".into());
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let (path, next) = take_bytes(rest)?;
        let host = PathBuf::from(String::from_utf8_lossy(&path).into_owned());
        rest = next;
        if rest.len() < 4 {
            return Err("truncated blob length".into());
        }
        let n = u32::from_le_bytes(rest[..4].try_into().unwrap_or([0; 4])) as usize;
        rest = &rest[4..];
        if n > 1024 * 1024 || rest.len() < n {
            return Err("truncated or oversized bookmark blob".into());
        }
        out.push((host, rest[..n].to_vec()));
        rest = &rest[n..];
    }
    Ok(out)
}

fn take_bytes(rest: &[u8]) -> Result<(Vec<u8>, &[u8]), String> {
    if rest.len() < 4 {
        return Err("truncated path length".into());
    }
    let n = u32::from_le_bytes(rest[..4].try_into().unwrap_or([0; 4])) as usize;
    if n > 64 * 1024 || rest.len() < 4 + n {
        return Err("truncated or oversized path".into());
    }
    Ok((rest[4..4 + n].to_vec(), &rest[4 + n..]))
}

#[cfg(test)]
mod tests {
    use super::{parse_store, MAGIC, VERSION};

    #[test]
    fn rejects_unknown_version() {
        let mut bytes = Vec::from(*MAGIC);
        bytes.extend_from_slice(&99u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        let err = parse_store(&bytes).unwrap_err();
        assert!(err.contains("unsupported version 99"), "{err}");
    }

    #[test]
    fn roundtrip_empty_blobs() {
        let mut bytes = Vec::from(*MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        let path = b"/tmp/disk.img";
        bytes.extend_from_slice(&(path.len() as u32).to_le_bytes());
        bytes.extend_from_slice(path);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        let entries = parse_store(&bytes).expect("parse");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0.as_os_str(), "/tmp/disk.img");
        assert!(entries[0].1.is_empty());
    }
}
