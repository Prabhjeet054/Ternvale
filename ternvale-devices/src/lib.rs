//! Emulated and virtio devices for Ternvale guests.
//!
//! Stub only. Devices will be exercised through the MMIO bus without a real VM where possible.
//! Public functions added here must carry
//! `#[tracing::instrument(level = "debug", skip_all, fields(...))]`
//! and log on `ternvale::mmio`, `ternvale::uart`, `ternvale::gic`, `ternvale::net`, or
//! `ternvale::virtio::<dev>`.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-devices");
    }
}
