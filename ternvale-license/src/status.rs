//! Entitlement after trial / license / grace.

use crate::payload::LicensePayload;

/// Default trial length.
pub const TRIAL_DAYS: u64 = 14;
/// Post-expiry / post-trial grace.
pub const GRACE_DAYS: u64 = 7;
const DAY: u64 = 86_400;

/// Why the user can or cannot start new VM sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entitlement {
    /// Paid license in date.
    Licensed {
        /// Claims.
        payload: LicensePayload,
    },
    /// Trial clock running.
    Trial {
        /// Trial start unix seconds.
        started_unix: u64,
        /// Exclusive trial end.
        ends_unix: u64,
    },
    /// License expired, still inside grace.
    Grace {
        /// Claims (may be absent in trial-only grace).
        payload: Option<LicensePayload>,
        /// Exclusive grace end.
        ends_unix: u64,
    },
    /// Open/export/backup only.
    Degraded {
        /// Human reason.
        reason: String,
    },
}

impl Entitlement {
    /// Create / start / clone / import new sessions.
    #[must_use]
    #[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
    pub fn allows_new_session(&self) -> bool {
        !matches!(self, Self::Degraded { .. })
    }

    /// Open, export, and back up existing VMs.
    #[must_use]
    #[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
    pub fn allows_open_export_backup(&self) -> bool {
        true
    }
}

/// Evaluate a verified license against `now`.
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn from_license(payload: LicensePayload, now: u64) -> Entitlement {
    if now < payload.expiry_unix {
        tracing::debug!(
            target: "ternvale::license",
            id = %payload.id,
            "licensed"
        );
        return Entitlement::Licensed { payload };
    }
    let grace_end = payload.expiry_unix.saturating_add(GRACE_DAYS * DAY);
    if now < grace_end {
        tracing::warn!(
            target: "ternvale::license",
            id = %payload.id,
            "license expired; grace remaining"
        );
        return Entitlement::Grace {
            payload: Some(payload),
            ends_unix: grace_end,
        };
    }
    tracing::warn!(
        target: "ternvale::license",
        id = %payload.id,
        "license expired; degraded (open/export/backup only)"
    );
    Entitlement::Degraded {
        reason: format!(
            "license {} expired; open, export, and backup remain available",
            payload.id
        ),
    }
}

/// Evaluate trial timestamps.
#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
pub fn from_trial(started_unix: u64, now: u64) -> Entitlement {
    let trial_end = started_unix.saturating_add(TRIAL_DAYS * DAY);
    if now < trial_end {
        tracing::debug!(target: "ternvale::license", "trial active");
        return Entitlement::Trial {
            started_unix,
            ends_unix: trial_end,
        };
    }
    let grace_end = trial_end.saturating_add(GRACE_DAYS * DAY);
    if now < grace_end {
        tracing::warn!(target: "ternvale::license", "trial ended; grace remaining");
        return Entitlement::Grace {
            payload: None,
            ends_unix: grace_end,
        };
    }
    tracing::warn!(
        target: "ternvale::license",
        "trial ended; degraded (open/export/backup only)"
    );
    Entitlement::Degraded {
        reason: "trial ended; open, export, and backup remain available".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{LicensePayload, PRODUCT};

    fn lic(expiry: u64) -> LicensePayload {
        LicensePayload {
            product: PRODUCT.into(),
            edition: "personal".into(),
            seats: 1,
            expiry_unix: expiry,
            issued_unix: 0,
            id: "x".into(),
        }
    }

    #[test]
    fn licensed_then_grace_then_degraded() {
        let p = lic(1_000);
        assert!(matches!(
            from_license(p.clone(), 999),
            Entitlement::Licensed { .. }
        ));
        assert!(matches!(
            from_license(p.clone(), 1_000),
            Entitlement::Grace { .. }
        ));
        let g = GRACE_DAYS * DAY;
        assert!(matches!(
            from_license(p.clone(), 1_000 + g - 1),
            Entitlement::Grace { .. }
        ));
        assert!(matches!(
            from_license(p, 1_000 + g),
            Entitlement::Degraded { .. }
        ));
    }

    #[test]
    fn degraded_never_blocks_export() {
        let e = Entitlement::Degraded { reason: "x".into() };
        assert!(!e.allows_new_session());
        assert!(e.allows_open_export_backup());
    }
}
