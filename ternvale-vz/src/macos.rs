//! Versioned macOS platform identity (`hardwareModel` + `machineIdentifier`).
//!
//! Without this file the guest cannot boot: Virtualization.framework binds
//! those blobs to `VZMacAuxiliaryStorage`. Magic `TVMP`, version 1.

use std::path::{Path, PathBuf};

use crate::error::VzError;

/// On-disk magic (`TVMP` = Ternvale VM macOS Platform).
pub const MAGIC: [u8; 4] = *b"TVMP";
/// Current layout.
pub const VERSION: u32 = 1;
/// File name next to `config.toml`.
pub const IDENTITY_NAME: &str = "macos-platform.tvmp";
/// `VZMacAuxiliaryStorage` file next to the identity.
pub const AUX_NAME: &str = "AuxiliaryStorage";

/// `hardwareModel` and `machineIdentifier` `dataRepresentation` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacosIdentity {
    /// [`VERSION`].
    pub version: u32,
    /// `VZMacHardwareModel.dataRepresentation`.
    pub hardware_model: Vec<u8>,
    /// `VZMacMachineIdentifier.dataRepresentation`.
    pub machine_identifier: Vec<u8>,
}

impl MacosIdentity {
    /// New identity at the current version.
    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all)]
    pub fn new(hardware_model: Vec<u8>, machine_identifier: Vec<u8>) -> Self {
        Self {
            version: VERSION,
            hardware_model,
            machine_identifier,
        }
    }

    /// Path of the identity file in a VM bundle directory.
    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all, fields(dir = %dir.display()))]
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(IDENTITY_NAME)
    }

    /// Auxiliary storage path in a VM bundle directory.
    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all, fields(dir = %dir.display()))]
    pub fn aux_path_in(dir: &Path) -> PathBuf {
        dir.join(AUX_NAME)
    }

    /// Encode little-endian on-disk form.
    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all)]
    pub fn encode(&self) -> Vec<u8> {
        let cap = 16 + self.hardware_model.len() + self.machine_identifier.len();
        let mut out = Vec::with_capacity(cap);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&(self.hardware_model.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.hardware_model);
        out.extend_from_slice(&(self.machine_identifier.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.machine_identifier);
        out
    }

    /// Parse; reject bad magic, unknown version, or truncation.
    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all)]
    pub fn decode(bytes: &[u8]) -> Result<Self, VzError> {
        decode_at(bytes, None)
    }

    /// Load from `path`.
    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all, fields(path = %path.display()))]
    pub fn load(path: &Path) -> Result<Self, VzError> {
        let bytes =
            std::fs::read(path).map_err(|error| platform_err(path, format!("read: {error}")))?;
        decode_at(&bytes, Some(path))
    }

    /// Write atomically (`*.tmp` then rename).
    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all, fields(path = %path.display()))]
    pub fn save(&self, path: &Path) -> Result<(), VzError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| platform_err(path, format!("create parent: {error}")))?;
        }
        let tmp = path.with_extension("tvmp.tmp");
        std::fs::write(&tmp, self.encode())
            .map_err(|error| platform_err(path, format!("write: {error}")))?;
        std::fs::rename(&tmp, path)
            .map_err(|error| platform_err(path, format!("rename: {error}")))?;
        tracing::info!(
            target: "ternvale::vz",
            path = %path.display(),
            hw = self.hardware_model.len(),
            id = self.machine_identifier.len(),
            "wrote macos platform identity"
        );
        Ok(())
    }

    /// Lowercase hex of the hardware model (C ABI / Swift).
    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all)]
    pub fn hardware_hex(&self) -> String {
        hex(&self.hardware_model)
    }

    /// Lowercase hex of the machine identifier.
    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all)]
    pub fn identifier_hex(&self) -> String {
        hex(&self.machine_identifier)
    }
}

/// Parse hex (even length, lowercase or upper).
#[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all)]
pub fn parse_hex(text: &str) -> Result<Vec<u8>, VzError> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return Err(VzError::Host {
            op: "macos_hex",
            detail: "odd-length hex".into(),
        });
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = nibble(bytes[i])?;
        let lo = nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn nibble(b: u8) -> Result<u8, VzError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(VzError::Host {
            op: "macos_hex",
            detail: "non-hex digit".into(),
        }),
    }
}

fn hex(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(D[(b >> 4) as usize] as char);
        out.push(D[(b & 0xf) as usize] as char);
    }
    out
}

fn decode_at(bytes: &[u8], path: Option<&Path>) -> Result<MacosIdentity, VzError> {
    let fail = |reason: String| {
        Err(platform_err(
            path.unwrap_or_else(|| Path::new(IDENTITY_NAME)),
            reason,
        ))
    };
    if bytes.len() < 12 {
        return fail("truncated macos platform identity (the VM cannot boot without it)".into());
    }
    if bytes[0..4] != MAGIC {
        return fail(
            "bad magic (expected TVMP); the macOS platform identity is missing or corrupt and the VM cannot boot"
                .into(),
        );
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap_or([0; 4]));
    if version != VERSION {
        return fail(format!(
            "unsupported macos platform version {version} (this build reads {VERSION}); the VM cannot boot"
        ));
    }
    let hw_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap_or([0; 4])) as usize;
    let hw_end = 12usize
        .checked_add(hw_len)
        .ok_or_else(|| platform_err(Path::new(IDENTITY_NAME), "hardwareModel length overflow"))?;
    if bytes.len() < hw_end + 4 {
        return fail("truncated hardwareModel".into());
    }
    let id_len =
        u32::from_le_bytes(bytes[hw_end..hw_end + 4].try_into().unwrap_or([0; 4])) as usize;
    let id_end = (hw_end + 4).checked_add(id_len).ok_or_else(|| {
        platform_err(
            Path::new(IDENTITY_NAME),
            "machineIdentifier length overflow",
        )
    })?;
    if bytes.len() < id_end {
        return fail("truncated machineIdentifier".into());
    }
    if hw_len == 0 || id_len == 0 {
        return fail("empty hardwareModel or machineIdentifier".into());
    }
    Ok(MacosIdentity {
        version,
        hardware_model: bytes[12..hw_end].to_vec(),
        machine_identifier: bytes[hw_end + 4..id_end].to_vec(),
    })
}

fn platform_err(path: &Path, reason: impl Into<String>) -> VzError {
    let reason = reason.into();
    tracing::error!(
        target: "ternvale::vz",
        path = %path.display(),
        reason = %reason,
        "macos platform identity unusable"
    );
    VzError::Host {
        op: "macos_platform",
        detail: format!("{}: {reason}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let id = MacosIdentity::new(vec![1, 2, 3, 4], vec![9, 8, 7, 6, 5]);
        let decoded = MacosIdentity::decode(&id.encode()).expect("decode");
        assert_eq!(decoded, id);
        assert_eq!(id.hardware_hex(), "01020304");
        assert_eq!(parse_hex("01020304").unwrap(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn bad_magic_names_boot() {
        let err = MacosIdentity::decode(b"NOPE\x01\x00\x00\x00\x00\x00\x00\x00").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("TVMP"), "{text}");
        assert!(text.contains("cannot boot"), "{text}");
    }

    #[test]
    fn unknown_version_is_rejected() {
        let mut bytes = MacosIdentity::new(vec![1], vec![2]).encode();
        bytes[4..8].copy_from_slice(&99u32.to_le_bytes());
        let text = MacosIdentity::decode(&bytes).unwrap_err().to_string();
        assert!(text.contains("version 99"), "{text}");
        assert!(text.contains("cannot boot"), "{text}");
    }

    #[test]
    fn truncated_is_rejected() {
        let err = MacosIdentity::decode(b"TVMP").unwrap_err();
        assert!(err.to_string().contains("truncated"), "{err}");
    }

    #[test]
    fn save_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("tv-macos-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = MacosIdentity::path_in(&dir);
        let id = MacosIdentity::new(vec![0xde, 0xad], vec![0xbe, 0xef]);
        id.save(&path).expect("save");
        let loaded = MacosIdentity::load(&path).expect("load");
        assert_eq!(loaded, id);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
