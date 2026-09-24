//! Command-line interface for the Ternvale VMM.
//!
//! Stub only. User-facing stdout lives here later; library logging uses `tracing`.
//! Public functions added here must carry
//! `#[tracing::instrument(level = "debug", skip_all, fields(...))]`
//! and log on the `ternvale::cli` target.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-cli");
    }
}
