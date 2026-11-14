//! Download a FEX-Emu x86 RootFS on explicit user request.
//!
//! Images are **not** bundled. The catalog `Hash` field is XXH3-64 with seed 0
//! (FEX `XXFileHash.cpp`). Ubuntu squashfs is Canonical's; see docs/X86.md.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use twox_hash::XxHash3_64;

use crate::json_out;
use crate::paths;

/// FEX RootFS index. TODO(verify) URL still matches FEX-Emu docs when bumping.
pub const CATALOG_URL: &str = "https://rootfs.fex-emu.gg/RootFS_links.json";

const LICENSE_NOTE: &str = "\
FEX-Emu is MIT (Ryan Houdek, 2019+). The RootFS squashfs is typically an
Ubuntu root filesystem from Canonical — it is NOT FEX's license and is NOT
shipped with Ternvale. Review Canonical's terms before you download. Type yes
to fetch, or anything else to cancel:";

/// `ternvale x86 fetch-rootfs`.
#[tracing::instrument(level = "info", target = "ternvale::compat", skip_all, fields(distro, dest = ?dest))]
pub fn fetch_rootfs(distro: &str, dest: Option<&Path>, json: bool) -> Result<ExitCode> {
    println!("{LICENSE_NOTE}");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("read FEX RootFS download consent")?;
    if !consented(&line) {
        tracing::warn!(target: "ternvale::compat", "FEX RootFS download cancelled");
        bail!("FEX RootFS download cancelled (type yes to accept the license note)");
    }
    let dest = match dest {
        Some(path) => path.to_path_buf(),
        None => default_dest(distro)?,
    };
    let simulate = std::env::var_os("TERNVALE_FEX_ROOTFS_SIMULATE").is_some();
    let wrote = fetch_to(&dest, distro, simulate)?;
    json_out::emit(
        json,
        serde_json::json!({"ok": true, "path": wrote.display().to_string(), "distro": distro}),
        || println!("FEX RootFS written to {}", wrote.display()),
    )?;
    Ok(ExitCode::SUCCESS)
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all)]
pub fn consented(line: &str) -> bool {
    line.trim() == "yes"
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all, fields(distro))]
fn default_dest(distro: &str) -> Result<PathBuf> {
    let safe: String = distro
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        bail!("distro name is empty");
    }
    Ok(paths::fex_rootfs_dir()?.join(format!("{safe}.sqsh")))
}

/// Fetch `distro` into `dest`. `simulate` writes a tiny local file (tests).
#[tracing::instrument(level = "info", target = "ternvale::compat", skip_all, fields(dest = %dest.display(), distro, simulate))]
pub fn fetch_to(dest: &Path, distro: &str, simulate: bool) -> Result<PathBuf> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create FEX RootFS dir {}", parent.display()))?;
    }
    if simulate {
        fs::write(dest, b"simulated-fex-rootfs\n")
            .with_context(|| format!("write simulated RootFS {}", dest.display()))?;
        tracing::info!(
            target: "ternvale::compat",
            dest = %dest.display(),
            "simulated FEX RootFS (no network)"
        );
        return Ok(dest.to_path_buf());
    }
    let catalog = curl_bytes(CATALOG_URL).context("download FEX RootFS catalog")?;
    let (url, want_hash) = catalog_entry(&catalog, distro)?;
    tracing::info!(
        target: "ternvale::compat",
        distro,
        url = %url,
        "downloading FEX RootFS"
    );
    let bytes = curl_bytes(&url).context("download FEX RootFS image")?;
    let got = xxh3_64_hex(&bytes);
    if !hash_matches(&got, &want_hash) {
        tracing::error!(
            target: "ternvale::compat",
            expected = %want_hash,
            got = %got,
            "FEX RootFS XXH3-64 mismatch"
        );
        bail!("FEX RootFS checksum mismatch (catalog Hash={want_hash}, got {got})");
    }
    let mut file =
        fs::File::create(dest).with_context(|| format!("create FEX RootFS {}", dest.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("write FEX RootFS {}", dest.display()))?;
    Ok(dest.to_path_buf())
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all, fields(url))]
fn curl_bytes(url: &str) -> Result<Vec<u8>> {
    let output = Command::new("curl")
        .args(["-fsSL", "--", url])
        .output()
        .context("spawn curl")?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        bail!("curl {url} failed: {err}");
    }
    Ok(output.stdout)
}

/// Pick `URL` and `Hash` for `distro` from the FEX catalog JSON object.
///
/// Current catalog is `{ "v1": { "<display name>": { URL, Hash, DistroMatch, ... } } }`.
/// `distro` may be that display name, a legacy id (`Ubuntu_24_04`), or `ubuntu-24.04`.
#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all, fields(distro))]
pub fn catalog_entry(json: &[u8], distro: &str) -> Result<(String, String)> {
    let value: Value = serde_json::from_slice(json).context("parse RootFS_links.json")?;
    let map = if let Some(v1) = value.get("v1").and_then(Value::as_object) {
        v1
    } else {
        value
            .as_object()
            .ok_or_else(|| anyhow!("RootFS catalog is not a JSON object"))?
    };
    let entry = map
        .get(distro)
        .or_else(|| find_catalog_entry(map, distro))
        .ok_or_else(|| {
            let keys: Vec<&str> = map.keys().map(String::as_str).collect();
            anyhow!("unknown FEX RootFS distro {distro:?}; catalog keys: {keys:?}")
        })?;
    let url = entry
        .get("URL")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("catalog entry {distro} missing URL"))?;
    let hash = entry
        .get("Hash")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("catalog entry {distro} missing Hash"))?;
    Ok((url.to_string(), hash.to_string()))
}

fn alnum(s: &str) -> String {
    s.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn find_catalog_entry<'a>(
    map: &'a serde_json::Map<String, Value>,
    distro: &str,
) -> Option<&'a Value> {
    let want = alnum(distro);
    let mut fallback = None;
    for (name, entry) in map {
        let dm = entry
            .get("DistroMatch")
            .and_then(Value::as_str)
            .unwrap_or("");
        let ver = entry
            .get("DistroVersion")
            .and_then(Value::as_str)
            .unwrap_or("");
        let typ = entry.get("Type").and_then(Value::as_str).unwrap_or("");
        let hit = alnum(name).contains(&want) || format!("{}{}", alnum(dm), alnum(ver)) == want;
        if !hit {
            continue;
        }
        if typ.eq_ignore_ascii_case("squashfs") || name.contains("SquashFS") {
            return Some(entry);
        }
        fallback = Some(entry);
    }
    fallback
}

#[tracing::instrument(level = "trace", target = "ternvale::compat", skip_all)]
pub fn xxh3_64_hex(bytes: &[u8]) -> String {
    format!("{:016x}", XxHash3_64::oneshot_with_seed(0, bytes))
}

fn hash_matches(got: &str, want: &str) -> bool {
    let want = want.trim().trim_start_matches("0x");
    got.eq_ignore_ascii_case(want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_is_exactly_yes() {
        assert!(consented("yes\n"));
        assert!(!consented("y\n"));
        assert!(!consented("YES\n"));
    }

    #[test]
    fn catalog_picks_named_entry() {
        let json = br#"{
            "v1": {
                "Ubuntu 24.04 (EroFS)": {
                    "DistroMatch": "ubuntu",
                    "DistroVersion": "24.04",
                    "URL": "https://example.test/u.ero",
                    "Hash": "00",
                    "Type": "erofs"
                },
                "Ubuntu 24.04 (SquashFS)": {
                    "DistroMatch": "ubuntu",
                    "DistroVersion": "24.04",
                    "URL": "https://example.test/u.sqsh",
                    "Hash": "aabbccddeeff0011",
                    "Type": "squashfs"
                }
            }
        }"#;
        let (url, hash) = catalog_entry(json, "Ubuntu_24_04").unwrap();
        assert_eq!(url, "https://example.test/u.sqsh");
        assert_eq!(hash, "aabbccddeeff0011");
        let (url2, _) = catalog_entry(json, "Ubuntu 24.04 (SquashFS)").unwrap();
        assert_eq!(url2, url);
        assert!(catalog_entry(json, "missing").is_err());
    }

    #[test]
    fn xxh3_seed_zero_is_stable() {
        let hex = xxh3_64_hex(b"ternvale-fex");
        assert_eq!(hex.len(), 16);
        assert_eq!(hex, xxh3_64_hex(b"ternvale-fex"));
        assert!(hash_matches(&hex, &hex));
        assert!(hash_matches(&hex, &format!("0x{hex}")));
    }

    #[test]
    fn simulate_writes_without_network() {
        let dir = std::env::temp_dir().join(format!(
            "ternvale-fex-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dest = dir.join("Ubuntu_24_04.sqsh");
        fetch_to(&dest, "Ubuntu_24_04", true).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"simulated-fex-rootfs\n");
        let _ = fs::remove_dir_all(dir);
    }
}
