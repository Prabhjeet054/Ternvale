//! Offline license verification, 14-day trial, and degraded-mode policy.
//!
//! The matching Ed25519 **private** key is never stored in this repository.
//! Log target: `ternvale::license`. Signing seeds are [`secret::SigningSeed`].

#![deny(missing_docs)]

pub mod activate;
pub mod blob;
pub mod check;
pub mod payload;
pub mod pubkey;
pub mod secret;
pub mod status;
pub mod trial;

mod error;
#[cfg(test)]
mod quality_fuzz;

pub use check::{current, evaluate, install_file, require_new_session, EvaluateInput};
pub use error::LicenseError;
pub use status::Entitlement;
