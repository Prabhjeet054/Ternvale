//! Emulated and virtio devices for Ternvale guests.

mod uart;

pub use uart::{ByteSink, Pl011, StdoutFile, UartError, PL011_BASE, PL011_SIZE};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-devices");
    }
}
