//! Emulated and virtio devices for Ternvale guests.

mod expect;
mod uart;

pub use expect::{last_lines, Progress, Session, Step};
pub use uart::{ByteSink, Pl011, StdoutFile, UartError, PL011_BASE, PL011_SIZE};

#[cfg(test)]
#[path = "boot_hv_test.rs"]
mod boot_hv_test;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-devices");
    }
}
