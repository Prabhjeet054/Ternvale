//! Compare the running helper version to a hosted (or local) update feed.
//!
//! The app fetches the same JSON over HTTPS. This command only reads a file so
//! we do not add an HTTP client. No download or install.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// Default HTTPS feed the macOS app queries (you must host this URL).
pub const DEFAULT_FEED_URL: &str = "https://updates.ternvale.dev/latest.json";

/// JSON document published at [`DEFAULT_FEED_URL`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UpdateFeed {
    /// `major.minor.patch` of the newest advertised build.
    pub version: String,
    /// Optional `CFBundleVersion`-style integer.
    pub build: Option<u32>,
    /// Human notes; unused for comparison.
    pub notes: Option<String>,
    /// Page to open; never treated as an installer payload.
    pub url: Option<String>,
}

/// `major.minor.patch` with missing parts treated as 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SemVer {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

/// Parse a dotted version; extra suffixes after `-` or `+` are dropped.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(text))]
pub fn parse_semver(text: &str) -> Result<SemVer> {
    let core = text.split(['-', '+']).next().unwrap_or(text).trim();
    let mut parts = core.split('.');
    let major = parse_part(parts.next(), "major", text)?;
    let minor = parse_part(parts.next(), "minor", text)?;
    let patch = parse_part(parts.next(), "patch", text)?;
    if parts.next().is_some() {
        tracing::warn!(target: "ternvale::cli", text, "ignoring extra version segments");
    }
    Ok(SemVer {
        major,
        minor,
        patch,
    })
}

fn parse_part(part: Option<&str>, name: &str, text: &str) -> Result<u32> {
    let Some(part) = part.filter(|p| !p.is_empty()) else {
        return Ok(0);
    };
    part.parse()
        .with_context(|| format!("version {text:?} {name} {part:?} is not a number"))
}

/// True when `remote` is strictly newer than `current`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(current, remote)
)]
pub fn is_newer(remote: &str, current: &str) -> Result<bool> {
    let newer = parse_semver(remote)? > parse_semver(current)?;
    tracing::debug!(target: "ternvale::cli", current, remote, newer, "compared versions");
    Ok(newer)
}

/// Decode a feed from bytes (file or HTTP body).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(bytes = json.len()))]
pub fn parse_feed(json: &str) -> Result<UpdateFeed> {
    let feed: UpdateFeed = serde_json::from_str(json).context("decode update feed JSON")?;
    if feed.version.trim().is_empty() {
        bail!("update feed version is empty");
    }
    parse_semver(&feed.version).context("validate feed version")?;
    tracing::info!(
        target: "ternvale::cli",
        version = %feed.version,
        build = ?feed.build,
        "parsed update feed"
    );
    Ok(feed)
}

/// Read `--feed` and print whether a newer version is advertised.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(feed = %feed.display()))]
pub fn update_check(feed: &Path) -> Result<std::process::ExitCode> {
    let current = env!("CARGO_PKG_VERSION");
    let text = std::fs::read_to_string(feed)
        .with_context(|| format!("read update feed {}", feed.display()))?;
    let remote = parse_feed(&text)?;
    let newer = is_newer(&remote.version, current)?;
    tracing::info!(
        target: "ternvale::cli",
        current,
        remote = %remote.version,
        newer,
        "update-check"
    );
    if newer {
        println!(
            "new version available: {} (running {current})",
            remote.version
        );
        if let Some(url) = &remote.url {
            println!("learn more: {url}");
        }
        Ok(std::process::ExitCode::from(2))
    } else {
        println!("up to date (running {current}, feed {})", remote.version);
        Ok(std::process::ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_patch_and_equal() {
        assert!(is_newer("0.1.1", "0.1.0").expect("ok"));
        assert!(!is_newer("0.1.0", "0.1.0").expect("ok"));
        assert!(!is_newer("0.1.0", "0.2.0").expect("ok"));
        assert!(is_newer("1.0.0", "0.9.9").expect("ok"));
    }

    #[test]
    fn parses_feed_and_rejects_empty_version() {
        let feed = parse_feed(
            r#"{"version":"0.2.0","build":2,"notes":"n","url":"https://updates.ternvale.dev/"}"#,
        )
        .expect("feed");
        assert_eq!(feed.version, "0.2.0");
        assert_eq!(feed.build, Some(2));
        assert!(parse_feed(r#"{"version":""}"#).is_err());
    }

    #[test]
    fn shipped_feed_matches_this_crate_version() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/updates/latest.json");
        let text = std::fs::read_to_string(&path).expect("docs/updates/latest.json");
        let feed = parse_feed(&text).expect("feed");
        assert_eq!(feed.version, env!("CARGO_PKG_VERSION"));
        assert!(!is_newer(&feed.version, env!("CARGO_PKG_VERSION")).expect("cmp"));
    }
}
