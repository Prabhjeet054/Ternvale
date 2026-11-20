//! Sanitize guest/host file names for clipboard and drag-and-drop.

use std::path::{Path, PathBuf};

/// Windows reserved device names (with or without an extension).
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Longest kept base name (bytes).
pub const MAX_SAFE_NAME: usize = 200;

/// Strip path components, `..`, control characters, and reserved Windows names.
#[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(raw_len = raw.len()))]
pub fn sanitize_file_name(raw: &str) -> String {
    let replaced = raw.replace(['\\', '/'], "/");
    let last = replaced
        .rsplit('/')
        .find(|part| !part.is_empty() && *part != "." && *part != "..")
        .unwrap_or("");
    let mut out = String::new();
    for ch in last.chars() {
        if ch.is_control()
            || ch == '<'
            || ch == '>'
            || ch == ':'
            || ch == '"'
            || ch == '|'
            || ch == '?'
            || ch == '*'
        {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    while out.ends_with(' ') || out.ends_with('.') {
        out.pop();
    }
    if out.is_empty() {
        tracing::warn!(target: "ternvale::config", "file name empty after sanitize; using unnamed");
        return "unnamed".into();
    }
    if out.len() > MAX_SAFE_NAME {
        out.truncate(MAX_SAFE_NAME);
    }
    if is_reserved(&out) {
        tracing::warn!(target: "ternvale::config", name = %out, "reserved Windows name; prefixing");
        out = format!("_{out}");
    }
    out
}

fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    RESERVED
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
}

/// `dir/name`, or `dir/stem-N.ext` when the path already exists.
#[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name))]
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let safe = sanitize_file_name(name);
    let candidate = dir.join(&safe);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = split_ext(&safe);
    for n in 1u32..10_000 {
        let next = if ext.is_empty() {
            dir.join(format!("{stem}-{n}"))
        } else {
            dir.join(format!("{stem}-{n}.{ext}"))
        };
        if !next.exists() {
            tracing::debug!(target: "ternvale::config", path = %next.display(), "unique dest");
            return next;
        }
    }
    dir.join(format!("{safe}.{}", std::process::id()))
}

fn split_ext(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => (stem, ext),
        _ => (name, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_traversal_and_reserved_names() {
        assert_eq!(sanitize_file_name(r"..\..\evil.txt"), "evil.txt");
        assert_eq!(sanitize_file_name("/tmp/../CON"), "_CON");
        assert_eq!(sanitize_file_name("con.txt"), "_con.txt");
        assert_eq!(sanitize_file_name("ok.png"), "ok.png");
        assert_eq!(sanitize_file_name("a\u{0001}b"), "a_b");
        assert_eq!(sanitize_file_name(".."), "unnamed");
        assert_eq!(sanitize_file_name(""), "unnamed");
    }

    #[test]
    fn unique_path_does_not_overwrite() {
        let dir = std::env::temp_dir().join(format!("ternvale-fn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("a.txt"), b"1").expect("write");
        let second = unique_path(&dir, "a.txt");
        assert_eq!(second.file_name().unwrap(), "a-1.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
