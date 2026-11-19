//! Collect KMDF/WPP sidecar logs from a rooted guest directory.

use std::fs;
use std::path::{Path, PathBuf};

use ternvale_agent_proto::MAX_DRIVER_LOG_BYTES;

/// Override with `TERNVALE_DRIVER_LOG_DIR` (tests and custom installs).
pub const ENV_DIR: &str = "TERNVALE_DRIVER_LOG_DIR";
/// Windows default (`%ProgramData%\Ternvale\driver-logs`).
pub const WINDOWS_DIR: &str = r"C:\ProgramData\Ternvale\driver-logs";
/// Linux default (harness / future Linux KMDF analog).
pub const UNIX_DIR: &str = "/var/log/ternvale/driver-logs";

/// Concatenated guest driver logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverLogs {
    /// UTF-8 text (lossy).
    pub text: String,
    /// True when `max_bytes` cut the concatenation short.
    pub truncated: bool,
}

/// Default log directory for this OS, unless [`ENV_DIR`] is set.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn log_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(ENV_DIR) {
        let path = PathBuf::from(dir);
        tracing::debug!(
            target: "ternvale::agent",
            path = %path.display(),
            "driver log dir from env"
        );
        return path;
    }
    if cfg!(windows) {
        PathBuf::from(WINDOWS_DIR)
    } else {
        PathBuf::from(UNIX_DIR)
    }
}

/// Read `*.log` / `*.txt` under `dir`, newest last, capped at `max_bytes`.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(dir = %dir.display(), max_bytes))]
pub fn collect(dir: &Path, max_bytes: usize) -> DriverLogs {
    let cap = max_bytes.min(MAX_DRIVER_LOG_BYTES);
    if cap == 0 {
        return DriverLogs {
            text: String::new(),
            truncated: false,
        };
    }
    let mut names = match fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| is_text_log(path))
            .collect::<Vec<_>>(),
        Err(error) => {
            tracing::debug!(
                target: "ternvale::agent",
                path = %dir.display(),
                error = %error,
                "driver log dir missing"
            );
            return DriverLogs {
                text: String::new(),
                truncated: false,
            };
        }
    };
    names.sort();
    let file_count = names.len();
    let mut text = String::new();
    let mut truncated = false;
    for path in &names {
        let body = match fs::read(path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => {
                tracing::warn!(
                    target: "ternvale::agent",
                    path = %path.display(),
                    error = %error,
                    "could not read driver log file"
                );
                continue;
            }
        };
        let header = format!("----- {} -----\n", file_name(path));
        for piece in [header.as_str(), body.as_str(), "\n"] {
            if text.len() >= cap {
                truncated = true;
                break;
            }
            let room = cap - text.len();
            if piece.len() > room {
                let mut end = room;
                while end > 0 && !piece.is_char_boundary(end) {
                    end -= 1;
                }
                text.push_str(&piece[..end]);
                truncated = true;
                break;
            }
            text.push_str(piece);
        }
        if truncated {
            break;
        }
    }
    tracing::info!(
        target: "ternvale::agent",
        files = file_count,
        bytes = text.len(),
        truncated,
        "collected guest driver logs"
    );
    DriverLogs { text, truncated }
}

fn is_text_log(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()),
        Some(ext) if ext == "log" || ext == "txt"
    )
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("tv-drvlog-{}-{}", std::process::id(), nanos));
        fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn concatenates_text_logs_and_caps() {
        let dir = scratch();
        fs::write(dir.join("a.log"), "one\n").expect("a");
        fs::write(dir.join("b.txt"), "two\n").expect("b");
        fs::write(dir.join("skip.etl"), b"\x00\x01").expect("etl");
        let all = collect(&dir, MAX_DRIVER_LOG_BYTES);
        assert!(all.text.contains("one"), "{}", all.text);
        assert!(all.text.contains("two"), "{}", all.text);
        assert!(!all.text.contains('\0'));
        assert!(!all.truncated);
        let tiny = collect(&dir, 8);
        assert!(tiny.truncated);
        assert!(tiny.text.len() <= 8);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_dir_is_empty() {
        let logs = collect(Path::new("/no/such/ternvale-driver-logs"), 1024);
        assert!(logs.text.is_empty());
        assert!(!logs.truncated);
    }
}
