//! Finding swtpm and checking that it supports Ternvale's invocation.
//!
//! `swtpm socket --print-capabilities` prints one JSON object with a
//! `features` array and (since swtpm 0.7) a `version` string (swtpm(8),
//! "--print-capabilities"). The feature names checked here are the ones the
//! man page lists for the options `process.rs` passes.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::SwtpmError;

/// Searched after `PATH`: Homebrew on Apple silicon, then on Intel. An app
/// launched from Finder gets a minimal `PATH` without them.
pub const SWTPM_FALLBACK_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// `--print-capabilities` features Ternvale's invocation relies on:
/// `--tpm2`, `--tpmstate backend-uri=file://…`, its `lock` parameter, and
/// `--ctrl …,terminate`.
pub const REQUIRED_FEATURES: [&str; 4] = [
    "tpm-2.0",
    "nvram-backend-file",
    "tpmstate-opt-lock",
    "ctrl-opt-terminate",
];

/// A swtpm binary that supports everything in [`REQUIRED_FEATURES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwtpmInfo {
    /// The binary.
    pub path: PathBuf,
    /// `version` from `--print-capabilities`.
    pub version: String,
    /// `features` from `--print-capabilities`.
    pub features: Vec<String>,
}

/// `explicit` if given, else the first executable `swtpm` on `PATH`, then in
/// [`SWTPM_FALLBACK_DIRS`].
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(explicit = ?explicit))]
pub fn locate_swtpm(explicit: Option<&Path>) -> Result<PathBuf, SwtpmError> {
    if let Some(path) = explicit {
        if is_executable(path) {
            tracing::debug!(target: "ternvale::tpm", path = %path.display(), "using the configured swtpm");
            return Ok(path.to_path_buf());
        }
        tracing::warn!(target: "ternvale::tpm", path = %path.display(), "configured swtpm is not an executable file");
        return Err(SwtpmError::NotFound {
            searched: path.display().to_string(),
        });
    }
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path_var)
        .chain(SWTPM_FALLBACK_DIRS.iter().map(PathBuf::from))
        .collect();
    for dir in &dirs {
        let candidate = dir.join("swtpm");
        if is_executable(&candidate) {
            tracing::debug!(target: "ternvale::tpm", path = %candidate.display(), "found swtpm");
            return Ok(candidate);
        }
    }
    tracing::debug!(target: "ternvale::tpm", dirs = dirs.len(), "swtpm not found");
    Err(SwtpmError::NotFound {
        searched: format!("PATH, {}", SWTPM_FALLBACK_DIRS.join(", ")),
    })
}

/// Run `path socket --print-capabilities` and check [`REQUIRED_FEATURES`].
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(path = %path.display()))]
pub fn probe_swtpm(path: &Path) -> Result<SwtpmInfo, SwtpmError> {
    let probe_error = |reason: String| {
        tracing::warn!(target: "ternvale::tpm", path = %path.display(), reason = %reason, "swtpm probe failed");
        SwtpmError::Probe {
            path: path.to_path_buf(),
            reason,
        }
    };
    let output = Command::new(path)
        .args(["socket", "--print-capabilities"])
        .output()
        .map_err(|error| probe_error(format!("could not run it: {error}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(probe_error(format!(
            "exited with {}: {}",
            output.status,
            stderr.trim()
        )));
    }
    let (version, features) = parse_capabilities(&stdout)
        .ok_or_else(|| probe_error(format!("no features list in {:?}", stdout.trim())))?;
    let missing: Vec<&str> = REQUIRED_FEATURES
        .iter()
        .copied()
        .filter(|feature| !features.iter().any(|f| f == feature))
        .collect();
    if !missing.is_empty() {
        tracing::warn!(target: "ternvale::tpm", path = %path.display(), version = %version, missing = ?missing, "swtpm lacks features Ternvale needs");
        return Err(SwtpmError::Unsupported {
            path: path.to_path_buf(),
            reason: format!(
                "swtpm {version} lacks {} (Ternvale needs {})",
                missing.join(", "),
                REQUIRED_FEATURES.join(", ")
            ),
        });
    }
    tracing::debug!(target: "ternvale::tpm", path = %path.display(), version = %version, features = features.len(), "swtpm probe passed");
    Ok(SwtpmInfo {
        path: path.to_path_buf(),
        version,
        features,
    })
}

/// [`locate_swtpm`] then [`probe_swtpm`].
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(explicit = ?explicit))]
pub fn find_swtpm(explicit: Option<&Path>) -> Result<SwtpmInfo, SwtpmError> {
    probe_swtpm(&locate_swtpm(explicit)?)
}

/// `(version, features)` from `--print-capabilities` JSON. A missing
/// `version` (swtpm before 0.7) reads as `"unknown"`. Plain string scanning:
/// the object is flat and its strings hold no escapes or brackets.
pub(super) fn parse_capabilities(json: &str) -> Option<(String, Vec<String>)> {
    let after = &json[json.find("\"features\"")?..];
    let list = &after[after.find('[')? + 1..];
    let list = &list[..list.find(']')?];
    let features = list
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_string())
        .filter(|item| !item.is_empty())
        .collect();
    let version = json
        .find("\"version\"")
        .and_then(|at| {
            let rest = &json[at + "\"version\"".len()..];
            let rest = &rest[rest.find('"')? + 1..];
            Some(rest[..rest.find('"')?].to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());
    Some((version, features))
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}
