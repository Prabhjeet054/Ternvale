//! Virtual xHCI USB 3 host controller for Ternvale guests.
//!
//! [`XhciDevice`] models the MMIO register file, command ring, event ring, a
//! fixed set of root ports, and transfer-ring handling for control and bulk
//! transfers. Two purely virtual HID devices (boot keyboard and mouse) provide
//! an input path that does not depend on virtio-input — useful for early
//! Windows boot before the virtio driver loads.
//!
//! Every TRB type is logged at TRACE on `ternvale::usb`. Wire the device on
//! the MMIO bus and inject reports through [`UsbInput`].

mod command;
mod event;
mod guest_mem;
mod hid;
mod host;
mod input;
mod input_ctx;
mod mem;
mod passthrough;
mod regs;
mod slots;
mod transfer;
mod trb;
mod xhci;

pub use guest_mem::{GuestMemoryAccess, MemError};
pub use hid::{BootKeyboardReport, BootMouseReport, HidKind};
pub use host::{HostUsbBackend, HostUsbError, UsbControlSetup};
pub use input::{InputError, UsbInput, UsbInputStats};
pub use passthrough::{ControlJob, PassthroughBinding, PassthroughWorker};
pub use regs::{MAX_SLOTS, MMIO_SIZE, NUM_PORTS};
pub use trb::{type_name as trb_type_name, Trb, TRB_SIZE};
pub use xhci::{attach_shared, attach_shared_with_passthrough, Xhci, XhciError};

#[cfg(test)]
mod host_tests;
#[cfg(test)]
mod xhci_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-usb");
    }
}
