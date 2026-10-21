//! Host-side USB backends for Ternvale (macOS IOKit).
//!
//! Claiming a physical device and servicing control/bulk transfers runs on a
//! worker thread; see `ternvale-usb::passthrough` and `docs/USB_PASSTHROUGH.md`.

mod ffi;
mod macos;

pub use macos::{claim_by_vid_pid, MacosHostUsb, MacosUsbError};

use ternvale_usb::HostUsbBackend;

/// Boxed backend ready to install on the xHCI passthrough worker.
pub type DynHostUsb = Box<dyn HostUsbBackend>;

/// Claim each configured device on the host. Failures are per-entry (logged); successful
/// claims are returned in config order.
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
pub fn claim_devices(
    entries: &[ternvale_config::UsbPassthroughEntry],
) -> Vec<(ternvale_config::UsbPassthroughEntry, DynHostUsb)> {
    let mut out = Vec::new();
    for entry in entries {
        match claim_by_vid_pid(entry.vendor_id, entry.product_id) {
            Ok(backend) => {
                tracing::info!(
                    target: "ternvale::usb",
                    vendor_id = format!("{:#06x}", entry.vendor_id),
                    product_id = format!("{:#06x}", entry.product_id),
                    port = entry.port,
                    "host usb device claimed for passthrough"
                );
                let boxed: DynHostUsb = Box::new(backend);
                out.push((entry.clone(), boxed));
            }
            Err(err) => {
                tracing::warn!(
                    target: "ternvale::usb",
                    vendor_id = format!("{:#06x}", entry.vendor_id),
                    product_id = format!("{:#06x}", entry.product_id),
                    error = %err,
                    "host usb claim failed; guest will not see this device"
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-usb-host");
    }
}
