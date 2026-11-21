//! Per-token rolling-window rate limit.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Counts requests per identity in the last minute.
pub struct RateLimiter {
    per_minute: u32,
    inner: Mutex<HashMap<String, Vec<Instant>>>,
}

impl RateLimiter {
    /// `per_minute` must be ≥ 1 (enforced by config).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gateway",
        skip_all,
        fields(per_minute)
    )]
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Record a hit. `false` means the caller is over the limit.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(id))]
    pub fn allow(&self, id: &str) -> bool {
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let Ok(mut map) = self.inner.lock() else {
            tracing::error!(target: "ternvale::gateway", "rate limiter lock poisoned");
            return false;
        };
        let stamps = map.entry(id.to_string()).or_default();
        stamps.retain(|t| now.saturating_duration_since(*t) < window);
        if stamps.len() as u32 >= self.per_minute {
            tracing::warn!(
                target: "ternvale::gateway",
                token_id = id,
                per_minute = self.per_minute,
                "rate limit exceeded"
            );
            return false;
        }
        stamps.push(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_then_blocks() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.allow("a"));
        assert!(limiter.allow("a"));
        assert!(!limiter.allow("a"));
        assert!(limiter.allow("b"));
    }
}
