//! 14-day trial start. Keychain on macOS; file fallback for tests.
//!
//! The start timestamp is not a security boundary. A determined user can
//! reset it. We still refuse to wipe VMs or otherwise punish honest users
//! who restore from backup or change clocks slightly.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::LicenseError;

/// Keychain service id for trial timestamps.
pub const KEYCHAIN_SERVICE: &str = "com.ternvale.license";
/// Account that stores the trial start unix seconds.
pub const KEYCHAIN_ACCOUNT: &str = "trial-start";

/// Last-seen unix time (clock rollback detection). Same Keychain service.
pub const KEYCHAIN_LAST_SEEN: &str = "last-seen";

/// Allowed backward clock jump before we freeze evaluation time (seconds).
pub const CLOCK_SKEW_SECS: u64 = 300;

/// Where trial bytes live (tests inject a directory).
pub trait TrialStore: Send + Sync {
    /// Read trial start unix, if any.
    fn load_start(&self) -> Result<Option<u64>, LicenseError>;
    /// Persist trial start once.
    fn save_start(&self, unix: u64) -> Result<(), LicenseError>;
    /// Last wall-clock observation.
    fn load_last_seen(&self) -> Result<Option<u64>, LicenseError>;
    /// Persist last seen.
    fn save_last_seen(&self, unix: u64) -> Result<(), LicenseError>;
}

/// In-memory store for unit tests.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<MemoryInner>,
}

#[derive(Debug, Default)]
struct MemoryInner {
    start: Option<u64>,
    last_seen: Option<u64>,
}

impl TrialStore for MemoryStore {
    #[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
    fn load_start(&self) -> Result<Option<u64>, LicenseError> {
        Ok(self.inner.lock().unwrap_or_else(|e| e.into_inner()).start)
    }

    #[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
    fn save_start(&self, unix: u64) -> Result<(), LicenseError> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).start = Some(unix);
        Ok(())
    }

    #[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
    fn load_last_seen(&self) -> Result<Option<u64>, LicenseError> {
        Ok(self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_seen)
    }

    #[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
    fn save_last_seen(&self, unix: u64) -> Result<(), LicenseError> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_seen = Some(unix);
        Ok(())
    }
}

/// Directory of two tiny files (`start`, `last_seen`). Used by tests and as
/// a fallback when Keychain is unavailable.
#[derive(Debug, Clone)]
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    /// `dir` is created on first write.
    #[must_use]
    #[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Directory that holds trial files and `license.tvlk`.
    #[must_use]
    #[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
    pub fn path(&self) -> &Path {
        &self.dir
    }

    fn read_u64(&self, name: &str) -> Result<Option<u64>, LicenseError> {
        let p = self.dir.join(name);
        match std::fs::read_to_string(&p) {
            Ok(s) => {
                let n = s.trim().parse::<u64>().map_err(|_| {
                    LicenseError::Version(format!("invalid trial file {}", p.display()))
                })?;
                Ok(Some(n))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(LicenseError::io("read trial file", e)),
        }
    }

    fn write_u64(&self, name: &str, n: u64) -> Result<(), LicenseError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| LicenseError::io("mkdir trial", e))?;
        let p = self.dir.join(name);
        std::fs::write(&p, format!("{n}\n")).map_err(|e| LicenseError::io("write trial file", e))
    }
}

impl TrialStore for FileStore {
    fn load_start(&self) -> Result<Option<u64>, LicenseError> {
        self.read_u64("start")
    }
    fn save_start(&self, unix: u64) -> Result<(), LicenseError> {
        self.write_u64("start", unix)
    }
    fn load_last_seen(&self) -> Result<Option<u64>, LicenseError> {
        self.read_u64("last_seen")
    }
    fn save_last_seen(&self, unix: u64) -> Result<(), LicenseError> {
        self.write_u64("last_seen", unix)
    }
}

/// Default trial directory under the user's logs/config root (tests override).
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn default_file_store() -> FileStore {
    let home = std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into());
    FileStore::new(Path::new(&home).join("Library/Application Support/Ternvale/license"))
}

/// Ensure a trial start exists; return it.
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn ensure_start(store: &dyn TrialStore, now: u64) -> Result<u64, LicenseError> {
    if let Some(s) = store.load_start()? {
        return Ok(s);
    }
    store.save_start(now)?;
    tracing::info!(
        target: "ternvale::license",
        now,
        "trial started (14 days)"
    );
    Ok(now)
}

/// If `now` is more than [`CLOCK_SKEW_SECS`] behind last seen, evaluate at
/// last_seen instead of punishing NTP corrections.
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn eval_now(store: &dyn TrialStore, wall: u64) -> Result<u64, LicenseError> {
    let last = store.load_last_seen()?.unwrap_or(wall);
    let eval = if wall + CLOCK_SKEW_SECS < last {
        tracing::warn!(
            target: "ternvale::license",
            wall,
            last,
            "clock moved backward; using last_seen for license evaluation"
        );
        last
    } else {
        wall
    };
    let persist = eval.max(last);
    store.save_last_seen(persist)?;
    Ok(eval)
}

/// macOS Keychain-backed store (`security` CLI). Falls back to [`FileStore`]
/// if `security` fails (CI without a keychain).
#[derive(Debug, Clone)]
pub struct KeychainStore {
    fallback: FileStore,
}

impl KeychainStore {
    /// Production store.
    #[must_use]
    #[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
    pub fn production() -> Self {
        Self {
            fallback: default_file_store(),
        }
    }

    fn security_find(&self, account: &str) -> Result<Option<u64>, LicenseError> {
        let out = std::process::Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                account,
                "-w",
            ])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout);
                let n = s
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| LicenseError::Version("invalid keychain trial value".into()))?;
                Ok(Some(n))
            }
            Ok(_) => Ok(None),
            Err(e) => {
                tracing::warn!(
                    target: "ternvale::license",
                    error = %e,
                    "security(1) missing; using file trial store"
                );
                self.fallback.read_u64(if account == KEYCHAIN_ACCOUNT {
                    "start"
                } else {
                    "last_seen"
                })
            }
        }
    }

    fn security_add(&self, account: &str, n: u64) -> Result<(), LicenseError> {
        let _ = std::process::Command::new("security")
            .args([
                "delete-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                account,
            ])
            .output();
        let status = std::process::Command::new("security")
            .args([
                "add-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                account,
                "-w",
                &n.to_string(),
                "-U",
            ])
            .status();
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(_) | Err(_) => {
                tracing::warn!(
                    target: "ternvale::license",
                    "keychain write failed; using file trial store"
                );
                let name = if account == KEYCHAIN_ACCOUNT {
                    "start"
                } else {
                    "last_seen"
                };
                self.fallback.write_u64(name, n)
            }
        }
    }
}

impl TrialStore for KeychainStore {
    fn load_start(&self) -> Result<Option<u64>, LicenseError> {
        self.security_find(KEYCHAIN_ACCOUNT)
    }
    fn save_start(&self, unix: u64) -> Result<(), LicenseError> {
        self.security_add(KEYCHAIN_ACCOUNT, unix)
    }
    fn load_last_seen(&self) -> Result<Option<u64>, LicenseError> {
        self.security_find(KEYCHAIN_LAST_SEEN)
    }
    fn save_last_seen(&self, unix: u64) -> Result<(), LicenseError> {
        self.security_add(KEYCHAIN_LAST_SEEN, unix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_start_once() {
        let s = MemoryStore::default();
        let a = ensure_start(&s, 10).expect("a");
        let b = ensure_start(&s, 99).expect("b");
        assert_eq!(a, 10);
        assert_eq!(b, 10);
    }

    #[test]
    fn clock_rollback_uses_last_seen() {
        let s = MemoryStore::default();
        s.save_last_seen(1_000_000).expect("ls");
        let eval = eval_now(&s, 100).expect("eval");
        assert_eq!(eval, 1_000_000);
    }
}
