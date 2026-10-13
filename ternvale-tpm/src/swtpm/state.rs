//! The TPM state file: swtpm's `backend-uri=file://` storage, its "linear"
//! format. swtpm `src/swtpm/swtpm_nvstore_linear.h` defines the header:
//!
//! | Offset | Size | Field |
//! | --- | --- | --- |
//! | 0 | 8 | magic, little-endian `0x737774706d6c696e` (`"swtpmlin"`) |
//! | 8 | 1 | version (`SWTPM_NVSTORE_LINEAR_VERSION`, 1) |
//! | 9 | 1 | padding |
//! | 10 | 2 | header size, little-endian (192 for version 1) |
//! | 12 | … | per-blob offset/length table, then the blobs |
//!
//! swtpm itself treats a file without the magic as empty and starts a new
//! TPM in it, which would give the guest a new TPM identity. So Ternvale
//! checks an existing file first: it must carry the magic and a version
//! Ternvale knows, or the VM does not start. A missing file is fine: swtpm
//! creates it and manufactures the TPM (new endorsement seeds) on first use.

use std::io::Read;
use std::path::Path;

use super::SwtpmError;

/// `SWTPM_NVSTORE_LINEAR_MAGIC`.
pub const STATE_MAGIC: u64 = 0x7377_7470_6d6c_696e;
/// `SWTPM_NVSTORE_LINEAR_VERSION` this code was checked against (swtpm
/// 0.10.2). TODO(verify) again before accepting a newer version.
pub const STATE_VERSION: u8 = 1;
/// Magic, version, padding, header size.
const PREFIX_LEN: usize = 12;

/// What [`check_state_file`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateFile {
    /// No file yet: swtpm will create one with a new TPM.
    New,
    /// A version-[`STATE_VERSION`] state file of `bytes` bytes.
    Existing {
        /// File size.
        bytes: u64,
    },
}

/// Check the state file at `path` before swtpm opens it.
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(path = %path.display()))]
pub fn check_state_file(path: &Path) -> Result<StateFile, SwtpmError> {
    let state_error = |reason: String| {
        tracing::error!(target: "ternvale::tpm", path = %path.display(), reason = %reason, "rejected tpm state file");
        SwtpmError::State {
            path: path.to_path_buf(),
            reason,
        }
    };
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent_ok = path
                .parent()
                .is_none_or(|dir| dir.as_os_str().is_empty() || dir.is_dir());
            if !parent_ok {
                return Err(state_error("its directory does not exist".into()));
            }
            tracing::info!(target: "ternvale::tpm", path = %path.display(), "no tpm state file yet; swtpm will create one with a new TPM identity");
            return Ok(StateFile::New);
        }
        Err(error) => return Err(state_error(format!("stat failed: {error}"))),
    };
    if !meta.is_file() {
        return Err(state_error("not a regular file".into()));
    }
    let mut prefix = [0u8; PREFIX_LEN];
    let read = std::fs::File::open(path).and_then(|mut file| file.read_exact(&mut prefix));
    if let Err(error) = read {
        return Err(state_error(format!(
            "{} bytes, cannot read the {PREFIX_LEN}-byte swtpm header ({error}); delete it only if a new TPM identity is acceptable",
            meta.len()
        )));
    }
    match parse_prefix(&prefix, meta.len()) {
        Ok(()) => {
            tracing::debug!(target: "ternvale::tpm", path = %path.display(), bytes = meta.len(), version = STATE_VERSION, "tpm state file header ok");
            Ok(StateFile::Existing { bytes: meta.len() })
        }
        Err(Prefix::Version(found)) => {
            tracing::error!(target: "ternvale::tpm", path = %path.display(), found, supported = STATE_VERSION, "tpm state file has an unknown swtpm format version");
            Err(SwtpmError::StateVersion {
                path: path.to_path_buf(),
                found,
                supported: STATE_VERSION,
            })
        }
        Err(Prefix::Other(reason)) => Err(state_error(reason)),
    }
}

/// Why a header prefix was rejected.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Prefix {
    Version(u8),
    Other(String),
}

/// Check magic, version and header size of a `len`-byte file.
pub(super) fn parse_prefix(prefix: &[u8; PREFIX_LEN], len: u64) -> Result<(), Prefix> {
    let magic = u64::from_le_bytes([
        prefix[0], prefix[1], prefix[2], prefix[3], prefix[4], prefix[5], prefix[6], prefix[7],
    ]);
    if magic != STATE_MAGIC {
        return Err(Prefix::Other(format!(
            "no swtpm linear-file magic \"swtpmlin\" (found {magic:#018x}); not a swtpm state file"
        )));
    }
    if prefix[8] != STATE_VERSION {
        return Err(Prefix::Version(prefix[8]));
    }
    let header = u64::from(u16::from_le_bytes([prefix[10], prefix[11]]));
    if header < PREFIX_LEN as u64 || header > len {
        return Err(Prefix::Other(format!(
            "header size {header} is outside {PREFIX_LEN}..={len} (the file is {len} bytes)"
        )));
    }
    Ok(())
}
