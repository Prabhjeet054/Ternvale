//! Host secrets (Windows password, product key). Never logged.

use std::fmt;

/// UTF-8 secret that prints `[redacted]` and is overwritten on drop.
pub struct Secret(String);

impl Secret {
    /// Wrap `value`. Empty is allowed (optional product key).
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrow the secret. Callers must not log the return value.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// True when there are no characters.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: we have exclusive ownership; filling with 0 keeps valid UTF-8.
        let bytes = unsafe { self.0.as_bytes_mut() };
        for byte in bytes.iter_mut() {
            *byte = 0;
        }
        self.0.clear();
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_are_redacted() {
        let secret = Secret::new("hunter2".into());
        assert_eq!(format!("{secret:?}"), "[redacted]");
        assert_eq!(format!("{secret}"), "[redacted]");
        assert_eq!(secret.expose(), "hunter2");
    }
}
