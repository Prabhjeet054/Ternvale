//! VM configuration and shared errors for Ternvale.
//!
//! Stub only. The TOML config model and `TernvaleError` come later.
//! Public functions added here must carry
//! `#[tracing::instrument(level = "debug", skip_all, fields(...))]`
//! and log on a `ternvale::…` target.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-config");
    }
}
