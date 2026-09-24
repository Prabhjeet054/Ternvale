//! Tracing setup, panic hook, and log files for Ternvale.
//!
//! Stub only. Subscriber installation and the panic hook come later.
//! Public functions added here must carry
//! `#[tracing::instrument(level = "debug", skip_all, fields(...))]`
//! and log on a `ternvale::…` target.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-log");
    }
}
