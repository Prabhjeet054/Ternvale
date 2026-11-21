//! Types that must never appear in logs.

use std::fmt;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Ed25519 seed / signing key bytes. `Debug` is always `[redacted]`.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SigningSeed(pub [u8; 32]);

impl fmt::Debug for SigningSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl fmt::Display for SigningSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}
