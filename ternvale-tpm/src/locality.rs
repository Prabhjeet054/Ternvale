//! Locality arbitration for the CRB `TPM_LOC_*` registers.
//!
//! PTP §6.5.3 (see [`crate::regs`]): software asks for a locality by writing
//! `requestAccess` to that locality's `TPM_LOC_CTRL_x`, gives it up with
//! `Relinquish`, and a higher locality may take it with `Seize`.
//! `TPM_LOC_STATE_x` (the same in every page) reports whether a locality is
//! assigned and which; `TPM_LOC_STS_x` reports `Granted` and `beenSeized` for
//! its own page.
//!
//! Model, after QEMU's `tpm_crb.c` extended to five localities:
//! - A request is granted at once when no locality is active, else it is kept
//!   pending and granted (highest locality first) when the active one
//!   relinquishes.
//! - Seize succeeds only from a locality above the active one; the loser's
//!   `beenSeized` is set and cleared when it is granted again.
//! - `resetEstablishmentBit` is accepted from localities 3 and 4 only and
//!   leaves `tpmEstablished` at 1 (no DRTM here).
//!
//! TODO(verify): pending-request ordering and when `beenSeized` clears are
//! read from the PTP's prose, not tested against a hardware TPM.

use crate::regs::{
    LOCALITIES, LOC_CTRL_RELINQUISH, LOC_CTRL_REQUEST_ACCESS, LOC_CTRL_RESET_ESTABLISHMENT,
    LOC_CTRL_SEIZE, LOC_STATE_ACTIVE_SHIFT, LOC_STATE_ASSIGNED, LOC_STATE_ESTABLISHED,
    LOC_STATE_REG_VALID, LOC_STS_BEEN_SEIZED, LOC_STS_GRANTED,
};

/// Which locality owns the TPM.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Localities {
    active: Option<u8>,
    pending: u8,
    seized: u8,
}

impl Localities {
    /// No locality assigned.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn new() -> Self {
        Self::default()
    }

    /// The active locality, if any.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn active(&self) -> Option<u8> {
        self.active
    }

    /// `TPM_LOC_STATE_x`.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn state(&self) -> u32 {
        let base = LOC_STATE_ESTABLISHED | LOC_STATE_REG_VALID;
        match self.active {
            Some(locality) => {
                base | LOC_STATE_ASSIGNED | (u32::from(locality) << LOC_STATE_ACTIVE_SHIFT)
            }
            None => base,
        }
    }

    /// `TPM_LOC_STS_x` for `locality`.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(locality))]
    pub fn status(&self, locality: u8) -> u32 {
        let granted = if self.active == Some(locality) {
            LOC_STS_GRANTED
        } else {
            0
        };
        let seized = if self.seized & bit(locality) != 0 {
            LOC_STS_BEEN_SEIZED
        } else {
            0
        };
        granted | seized
    }

    /// Apply a `TPM_LOC_CTRL_x` write from `locality`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::tpm",
        skip_all,
        fields(locality, value = %format!("{value:#x}"))
    )]
    pub fn control(&mut self, locality: u8, value: u32) {
        if locality >= LOCALITIES {
            tracing::warn!(target: "ternvale::tpm", locality, "locality control for a locality that does not exist");
            return;
        }
        let known = LOC_CTRL_REQUEST_ACCESS
            | LOC_CTRL_RELINQUISH
            | LOC_CTRL_SEIZE
            | LOC_CTRL_RESET_ESTABLISHMENT;
        if value & !known != 0 {
            tracing::warn!(
                target: "ternvale::tpm",
                locality,
                value = %format!("{value:#x}"),
                "reserved TPM_LOC_CTRL bits ignored"
            );
        }
        let before = self.active;
        if value & LOC_CTRL_RELINQUISH != 0 {
            self.relinquish(locality);
        }
        if value & LOC_CTRL_REQUEST_ACCESS != 0 {
            self.request(locality);
        }
        if value & LOC_CTRL_SEIZE != 0 {
            self.seize(locality);
        }
        if value & LOC_CTRL_RESET_ESTABLISHMENT != 0 {
            if locality >= 3 {
                tracing::debug!(target: "ternvale::tpm", locality, "resetEstablishmentBit: tpmEstablished stays 1 (no DRTM)");
            } else {
                tracing::warn!(target: "ternvale::tpm", locality, "resetEstablishmentBit from locality below 3 ignored");
            }
        }
        if self.active != before {
            tracing::debug!(
                target: "ternvale::tpm",
                from = ?before,
                to = ?self.active,
                pending = %format!("{:#07b}", self.pending),
                "tpm active locality changed"
            );
        }
    }

    fn request(&mut self, locality: u8) {
        match self.active {
            None => self.grant(locality),
            Some(active) if active == locality => {}
            Some(active) => {
                self.pending |= bit(locality);
                tracing::debug!(target: "ternvale::tpm", locality, active, "locality request pending");
            }
        }
    }

    fn relinquish(&mut self, locality: u8) {
        self.pending &= !bit(locality);
        if self.active != Some(locality) {
            return;
        }
        self.active = None;
        if let Some(next) = (0..LOCALITIES).rev().find(|l| self.pending & bit(*l) != 0) {
            self.pending &= !bit(next);
            self.grant(next);
        }
    }

    fn seize(&mut self, locality: u8) {
        match self.active {
            None => self.grant(locality),
            Some(active) if locality > active => {
                self.seized |= bit(active);
                self.pending &= !bit(locality);
                self.grant(locality);
                tracing::debug!(target: "ternvale::tpm", locality, seized = active, "locality seized");
            }
            Some(active) => {
                tracing::warn!(target: "ternvale::tpm", locality, active, "seize from a locality not above the active one ignored");
            }
        }
    }

    fn grant(&mut self, locality: u8) {
        self.active = Some(locality);
        self.seized &= !bit(locality);
    }
}

fn bit(locality: u8) -> u8 {
    1 << locality
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_grants_when_free_and_relinquish_frees() {
        let mut l = Localities::new();
        assert_eq!(l.state(), LOC_STATE_ESTABLISHED | LOC_STATE_REG_VALID);
        l.control(0, LOC_CTRL_REQUEST_ACCESS);
        assert_eq!(l.active(), Some(0));
        assert_eq!(
            l.state(),
            0b1000_0011,
            "valid, assigned, established, locality 0"
        );
        assert_eq!(l.status(0), LOC_STS_GRANTED);
        assert_eq!(l.status(1), 0);
        l.control(0, LOC_CTRL_RELINQUISH);
        assert_eq!(l.active(), None);
        assert_eq!(l.status(0), 0);
    }

    #[test]
    fn active_locality_is_reported_in_bits_4_to_2() {
        let mut l = Localities::new();
        l.control(4, LOC_CTRL_REQUEST_ACCESS);
        assert_eq!(l.state() >> LOC_STATE_ACTIVE_SHIFT & 0b111, 4);
    }

    #[test]
    fn pending_requests_are_granted_highest_first_on_relinquish() {
        let mut l = Localities::new();
        l.control(0, LOC_CTRL_REQUEST_ACCESS);
        l.control(1, LOC_CTRL_REQUEST_ACCESS);
        l.control(2, LOC_CTRL_REQUEST_ACCESS);
        assert_eq!(l.active(), Some(0));
        l.control(0, LOC_CTRL_RELINQUISH);
        assert_eq!(l.active(), Some(2));
        l.control(2, LOC_CTRL_RELINQUISH);
        assert_eq!(l.active(), Some(1));
    }

    #[test]
    fn seize_needs_a_higher_locality_and_marks_the_loser() {
        let mut l = Localities::new();
        l.control(2, LOC_CTRL_REQUEST_ACCESS);
        l.control(1, LOC_CTRL_SEIZE);
        assert_eq!(l.active(), Some(2), "lower locality cannot seize");
        l.control(3, LOC_CTRL_SEIZE);
        assert_eq!(l.active(), Some(3));
        assert_eq!(l.status(2), LOC_STS_BEEN_SEIZED);
        l.control(3, LOC_CTRL_RELINQUISH);
        l.control(2, LOC_CTRL_REQUEST_ACCESS);
        assert_eq!(l.status(2), LOC_STS_GRANTED, "beenSeized clears on grant");
    }

    #[test]
    fn bad_localities_and_establishment_resets_change_nothing() {
        let mut l = Localities::new();
        l.control(5, LOC_CTRL_REQUEST_ACCESS);
        assert_eq!(l, Localities::new());
        l.control(0, LOC_CTRL_RESET_ESTABLISHMENT);
        l.control(4, LOC_CTRL_RESET_ESTABLISHMENT | 0x100);
        assert_eq!(l, Localities::new());
        assert_ne!(l.state() & LOC_STATE_ESTABLISHED, 0);
    }
}
