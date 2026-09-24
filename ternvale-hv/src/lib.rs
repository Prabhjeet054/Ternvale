//! Safe wrappers around Apple's Hypervisor.framework.
//!
//! Stub only. Raw FFI will live only in this crate.
//! Public functions added here must carry
//! `#[tracing::instrument(level = "debug", skip_all, fields(...))]`
//! and log on the `ternvale::hv` target.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-hv");
    }

    /// Placeholder so `make test-hv` has one signed binary to execute.
    /// Hypervisor.framework calls are not made here.
    #[test]
    #[ignore = "needs-hv"]
    fn hypervisor_entitlement_is_present() {
        assert!(true);
    }
}
