//! Virtual machine lifecycle, vCPU, and memory orchestration.
//!
//! Stub only. This crate will sit between the CLI and `ternvale-hv` / `ternvale-devices`.
//! Public functions added here must carry
//! `#[tracing::instrument(level = "debug", skip_all, fields(...))]`
//! and log on a `ternvale::…` target (`mem`, `vcpu`, `boot`, and others as they appear).

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-vmm");
    }
}
