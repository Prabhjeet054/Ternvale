//! Bearer tokens with hashed storage and scopes. Secrets are never logged.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::GatewayError;

/// Magic for the token store (`TVTK` v1).
pub const TOKEN_MAGIC: &[u8; 4] = b"TVTK";
/// Current token-store version.
pub const TOKEN_VERSION: u32 = 1;

/// Privilege of a bearer token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// GET list/stats/snapshots/screenshot/events/serial.
    ReadOnly,
    /// Plus lifecycle, balloon, snapshot create/restore.
    Operator,
    /// Plus create/delete/clone.
    Admin,
}

impl Scope {
    /// Wire name.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Operator => "operator",
            Self::Admin => "admin",
        }
    }

    /// Parse a wire name.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(name))]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "read-only" | "readonly" => Some(Self::ReadOnly),
            "operator" => Some(Self::Operator),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }

    /// Rank for comparison (higher is more privilege).
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
    pub fn rank(self) -> u8 {
        match self {
            Self::ReadOnly => 1,
            Self::Operator => 2,
            Self::Admin => 3,
        }
    }

    /// True when this token may perform `needed`.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
    pub fn allows(self, needed: Scope) -> bool {
        self.rank() >= needed.rank()
    }
}

/// Bearer secret: `Debug`/`Display` print `[redacted]`.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct TokenSecret(String);

impl TokenSecret {
    /// Wrap a token string.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrow the secret. Callers must not log this.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for TokenSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl std::fmt::Display for TokenSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[redacted]")
    }
}

/// One hashed token record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRecord {
    /// Public id (hex), also the rate-limit key.
    pub id: String,
    /// SHA-256 of the secret, lowercase hex.
    pub hash: String,
    /// Privilege.
    pub scope: Scope,
    /// Unix expiry seconds, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_unix: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenFile {
    tokens: Vec<TokenRecord>,
}

/// Authenticated caller (token id is not secret).
#[derive(Debug, Clone)]
pub struct Identity {
    /// Token id.
    pub id: String,
    /// Privilege.
    pub scope: Scope,
    /// `mtls` or `bearer`.
    pub via: &'static str,
}

/// On-disk hashed token list.
pub struct TokenStore {
    path: PathBuf,
    records: Vec<TokenRecord>,
}

impl TokenStore {
    /// Load `path` (`TVTK` v1). Missing file is an empty store.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(path = %path.as_ref().display()))]
    pub fn load(path: impl AsRef<Path>) -> Result<Self, GatewayError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            tracing::info!(
                target: "ternvale::gateway",
                path = %path.display(),
                "token store missing; starting empty"
            );
            return Ok(Self {
                path,
                records: Vec::new(),
            });
        }
        let bytes = fs::read(&path)
            .map_err(|source| GatewayError::io_path("read token store", path.display(), source))?;
        let records = decode_store(&bytes)?;
        tracing::info!(
            target: "ternvale::gateway",
            path = %path.display(),
            count = records.len(),
            "token store loaded"
        );
        Ok(Self { path, records })
    }

    /// Persist `TVTK` v1 (mode 0600).
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
    pub fn save(&self) -> Result<(), GatewayError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| {
                GatewayError::io_path("create token store directory", parent.display(), source)
            })?;
        }
        let bytes = encode_store(&self.records)?;
        fs::write(&self.path, bytes).map_err(|source| {
            GatewayError::io_path("write token store", self.path.display(), source)
        })?;
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600)).map_err(|source| {
            GatewayError::io_path("chmod token store", self.path.display(), source)
        })?;
        Ok(())
    }

    /// Issue a token; the secret is returned once and not stored.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(scope = scope.as_str()))]
    pub fn issue(
        &mut self,
        scope: Scope,
        expires_unix: Option<u64>,
    ) -> Result<(String, TokenSecret), GatewayError> {
        let mut id_bytes = [0u8; 8];
        let mut secret_bytes = [0u8; 24];
        getrandom::getrandom(&mut id_bytes).map_err(|_| GatewayError::Rng)?;
        getrandom::getrandom(&mut secret_bytes).map_err(|_| GatewayError::Rng)?;
        let id = hex(&id_bytes);
        let secret_hex = hex(&secret_bytes);
        let hash = hash_secret(&secret_hex);
        self.records.push(TokenRecord {
            id: id.clone(),
            hash,
            scope,
            expires_unix,
        });
        self.save()?;
        let wire = format!("tvt_{id}.{secret_hex}");
        tracing::info!(
            target: "ternvale::gateway",
            token_id = %id,
            scope = scope.as_str(),
            "token issued"
        );
        Ok((id, TokenSecret::new(wire)))
    }

    /// Validate `Authorization: Bearer`.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
    pub fn authenticate(&self, header: Option<&str>, now_unix: u64) -> Result<Identity, AuthFail> {
        let value = header.ok_or(AuthFail::Missing)?;
        let token = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
            .ok_or(AuthFail::Malformed)?;
        let (id, secret) = parse_wire(token).ok_or(AuthFail::Malformed)?;
        let record = self
            .records
            .iter()
            .find(|row| row.id == id)
            .ok_or(AuthFail::Unknown)?;
        let expected = decode_hex(&record.hash).ok_or(AuthFail::Unknown)?;
        let actual = Sha256::digest(secret.as_bytes());
        if !constant_eq(&expected, actual.as_slice()) {
            return Err(AuthFail::Unknown);
        }
        if let Some(expiry) = record.expires_unix {
            if now_unix >= expiry {
                return Err(AuthFail::Expired);
            }
        }
        Ok(Identity {
            id: record.id.clone(),
            scope: record.scope,
            via: "bearer",
        })
    }
}

/// Why a bearer was rejected (no secret material).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFail {
    /// No Authorization header.
    Missing,
    /// Not `Bearer tvt_id.secret`.
    Malformed,
    /// Unknown id or bad hash.
    Unknown,
    /// Past `expires_unix`.
    Expired,
}

impl AuthFail {
    /// HTTP reason (no secrets).
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing bearer token",
            Self::Malformed => "malformed bearer token",
            Self::Unknown => "unknown bearer token",
            Self::Expired => "expired bearer token",
        }
    }
}

/// Unix time seconds.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn parse_wire(token: &str) -> Option<(&str, &str)> {
    let rest = token.strip_prefix("tvt_")?;
    let (id, secret) = rest.split_once('.')?;
    if id.is_empty() || secret.is_empty() {
        return None;
    }
    Some((id, secret))
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn hash_secret(secret: &str) -> String {
    hex(Sha256::digest(secret.as_bytes()).as_slice())
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn encode_store(records: &[TokenRecord]) -> Result<Vec<u8>, GatewayError> {
    let json = serde_json::to_vec(&TokenFile {
        tokens: records.to_vec(),
    })?;
    let mut out = Vec::with_capacity(8 + json.len());
    out.extend_from_slice(TOKEN_MAGIC);
    out.extend_from_slice(&TOKEN_VERSION.to_le_bytes());
    out.extend_from_slice(&json);
    Ok(out)
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn decode_store(bytes: &[u8]) -> Result<Vec<TokenRecord>, GatewayError> {
    if bytes.len() < 8 {
        return Err(GatewayError::Version(
            "token store too short (need TVTK v1 header)".into(),
        ));
    }
    if &bytes[0..4] != TOKEN_MAGIC {
        return Err(GatewayError::Version(
            "token store magic is not TVTK".into(),
        ));
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap_or([0; 4]));
    if version != TOKEN_VERSION {
        return Err(GatewayError::Version(format!(
            "unsupported token store version {version} (want {TOKEN_VERSION})"
        )));
    }
    let file: TokenFile = serde_json::from_slice(&bytes[8..])?;
    Ok(file.tokens)
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = from_hex(bytes[i])?;
        let lo = from_hex(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in a.iter().zip(b.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_redacted() {
        let secret = TokenSecret::new("tvt_dead.beef".into());
        assert_eq!(format!("{secret:?}"), "[redacted]");
        assert_eq!(format!("{secret}"), "[redacted]");
        assert_eq!(secret.expose(), "tvt_dead.beef");
    }

    #[test]
    fn issue_and_authenticate_round_trip() {
        let dir = std::env::temp_dir().join(format!("tvtk-{}-{}", std::process::id(), "auth"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("tokens.tvtk");
        let mut store = TokenStore::load(&path).expect("load");
        let (id, secret) = store.issue(Scope::Operator, None).expect("issue");
        let header = format!("Bearer {}", secret.expose());
        let ident = store.authenticate(Some(&header), now_unix()).expect("auth");
        assert_eq!(ident.id, id);
        assert_eq!(ident.scope, Scope::Operator);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn expired_token_is_rejected() {
        let dir = std::env::temp_dir().join(format!("tvtk-{}-{}", std::process::id(), "exp"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("tokens.tvtk");
        let mut store = TokenStore::load(&path).expect("load");
        let (_id, secret) = store.issue(Scope::ReadOnly, Some(1)).expect("issue");
        let header = format!("Bearer {}", secret.expose());
        assert_eq!(
            store.authenticate(Some(&header), 2).expect_err("expired"),
            AuthFail::Expired
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_version_is_rejected() {
        let mut bytes = Vec::from(*TOKEN_MAGIC);
        bytes.extend_from_slice(&99u32.to_le_bytes());
        bytes.extend_from_slice(b"{}");
        let err = decode_store(&bytes).expect_err("version");
        assert!(err.to_string().contains("unsupported token store version"));
    }
}
