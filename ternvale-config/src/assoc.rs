//! Share-rooted path translation and URL-scheme checks for guest apps.

use std::path::{Path, PathBuf};

use crate::{AgentSection, Share};

/// Extensions that must never be handed to the macOS default handler as
/// an “open file” (they would execute).
pub const BLOCKED_EXEC_EXTS: [&str; 14] = [
    "sh", "command", "exe", "bat", "cmd", "ps1", "app", "msi", "scr", "com", "jar", "bash", "zsh",
    "fish",
];

/// Map a host file onto a guest share path. Never copies.
#[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(host = %host.display()))]
pub fn host_file_to_guest(shares: &[Share], host: &Path) -> Result<String, String> {
    let host = canonicalize_existing(host)?;
    if host
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("host path must not contain '..'".into());
    }
    for share in shares {
        let root = match std::fs::canonicalize(&share.host_path) {
            Ok(root) => root,
            Err(_) => continue,
        };
        if let Ok(rel) = host.strip_prefix(&root) {
            if rel
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                continue;
            }
            let guest = join_guest(&share.mount_point, rel);
            tracing::debug!(
                target: "ternvale::config",
                tag = %share.tag,
                guest = %guest,
                "mapped host file onto share"
            );
            return Ok(guest);
        }
    }
    Err("file is not on a shared folder; Ternvale will not copy it silently".into())
}

/// Map a guest share path back to a host file. Never copies.
#[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(guest))]
pub fn guest_file_to_host(shares: &[Share], guest: &str) -> Result<PathBuf, String> {
    if guest.contains('\0') || guest.split(['/', '\\']).any(|part| part == "..") {
        return Err("guest path must not contain NUL or '..'".into());
    }
    let guest_norm = guest.replace('\\', "/");
    for share in shares {
        let mount = share.mount_point.replace('\\', "/");
        let mount_trim = mount.trim_end_matches('/');
        let rest = if guest_norm.eq_ignore_ascii_case(mount_trim) {
            Path::new("")
        } else if let Some(rest) = strip_mount(&guest_norm, mount_trim) {
            Path::new(rest)
        } else {
            continue;
        };
        if rest
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err("guest path escaped the share".into());
        }
        let host = share.host_path.join(rest);
        let canon = canonicalize_existing(&host)?;
        let root = std::fs::canonicalize(&share.host_path).map_err(|error| {
            format!("canonicalize share {}: {error}", share.host_path.display())
        })?;
        if !canon.starts_with(&root) {
            return Err("guest path escaped the share".into());
        }
        tracing::debug!(
            target: "ternvale::config",
            tag = %share.tag,
            host = %canon.display(),
            "mapped guest file onto share"
        );
        return Ok(canon);
    }
    Err("guest path is not on a shared folder".into())
}

/// Scheme of `url` (`http` from `https://…`). None if missing.
#[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
pub fn url_scheme(url: &str) -> Option<&str> {
    let (scheme, rest) = url.split_once(':')?;
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '+' || ch == '.' || ch == '-')
    {
        return None;
    }
    if rest.starts_with("//") || scheme.eq_ignore_ascii_case("mailto") || !rest.is_empty() {
        return Some(scheme);
    }
    Some(scheme)
}

/// Policy for guest → host URL open.
#[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(url))]
pub fn check_host_url(policy: &AgentSection, url: &str) -> Result<String, String> {
    if !policy.open_on_host {
        return Err("guest → host open is disabled (set agent.open_on_host = true)".into());
    }
    let Some(scheme) = url_scheme(url) else {
        return Err("URL has no scheme".into());
    };
    if scheme.eq_ignore_ascii_case("file") {
        return Err("file: URLs are never opened on the host".into());
    }
    if !policy.scheme_allowed(scheme) {
        return Err(format!(
            "URL scheme {scheme} is not on this VM's allowlist (default http, https, mailto)"
        ));
    }
    Ok(scheme.to_ascii_lowercase())
}

/// True when this extension must not be auto-opened.
#[tracing::instrument(level = "trace", target = "ternvale::config", skip_all, fields(ext))]
pub fn is_blocked_exec_ext(ext: &str) -> bool {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    BLOCKED_EXEC_EXTS.iter().any(|blocked| *blocked == ext)
}

/// File extension without a leading dot (empty if none).
#[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
pub fn file_ext(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn canonicalize_existing(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn join_guest(mount: &str, rel: &Path) -> String {
    let rel = rel.to_string_lossy().replace('/', "\\");
    if mount.len() >= 2 && mount.as_bytes()[1] == b':' {
        let mount = mount.trim_end_matches(['\\', '/']);
        if rel.is_empty() {
            format!("{mount}\\")
        } else {
            format!("{mount}\\{rel}")
        }
    } else {
        let mount = mount.trim_end_matches('/');
        let rel = rel.replace('\\', "/");
        if rel.is_empty() {
            mount.to_string()
        } else {
            format!("{mount}/{rel}")
        }
    }
}

fn strip_mount<'a>(guest: &'a str, mount: &str) -> Option<&'a str> {
    let g = guest.as_bytes();
    let m = mount.as_bytes();
    if g.len() < m.len() {
        return None;
    }
    if !g[..m.len()].eq_ignore_ascii_case(m) {
        return None;
    }
    let rest = &guest[m.len()..];
    if rest.is_empty() {
        return Some("");
    }
    if rest.starts_with('/') || rest.starts_with('\\') {
        return Some(rest.trim_start_matches(['/', '\\']));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn share(dir: &Path, mount: &str) -> Share {
        Share {
            host_path: dir.to_path_buf(),
            tag: "host".into(),
            mount_point: mount.into(),
            read_only: false,
        }
    }

    #[test]
    fn host_and_guest_round_trip_unix_mount() {
        let dir = std::env::temp_dir().join(format!("tv-assoc-{}", std::process::id()));
        let nested = dir.join("docs");
        fs::create_dir_all(&nested).expect("dir");
        let file = nested.join("note.txt");
        fs::write(&file, b"hi").expect("write");
        let shares = [share(&dir, "/mnt/ternvale")];
        let guest = host_file_to_guest(&shares, &file).expect("map");
        assert_eq!(guest, "/mnt/ternvale/docs/note.txt");
        let back = guest_file_to_host(&shares, &guest).expect("back");
        assert_eq!(back, fs::canonicalize(&file).expect("canon"));
        assert!(host_file_to_guest(&shares, Path::new("/tmp")).is_err());
        assert!(guest_file_to_host(&shares, "/mnt/ternvale/../etc/passwd").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn url_policy_denies_file_and_unknown_schemes() {
        let mut policy = AgentSection::default();
        policy.open_on_host = true;
        assert!(check_host_url(&policy, "https://example.com").is_ok());
        assert!(check_host_url(&policy, "mailto:a@b.c").is_ok());
        assert!(check_host_url(&policy, "file:///tmp/x").is_err());
        assert!(check_host_url(&policy, "slack://open").is_err());
        policy.url_schemes.push("slack".into());
        assert!(check_host_url(&policy, "slack://open").is_ok());
        policy.open_on_host = false;
        assert!(check_host_url(&policy, "https://example.com").is_err());
        assert!(is_blocked_exec_ext("command"));
        assert!(!is_blocked_exec_ext("txt"));
    }
}
