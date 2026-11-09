//! Host-side USB backends for Ternvale (macOS IOKit).
//!
//! Claiming a physical device and servicing control/bulk transfers runs on a
//! worker thread; see `ternvale-usb::passthrough` and `docs/USB_PASSTHROUGH.md`.

mod ffi;
mod helper_client;
mod helper_proto;
mod macos;

pub use helper_client::HelperHostUsb;
pub use helper_proto::{read_claim, read_xfer, write_ack, write_reply, CLAIM_MAGIC};
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
    let via_helper = std::env::var("TERNVALE_SANDBOXED").is_ok_and(|v| v == "1")
        || std::env::var("TERNVALE_USB_VIA_HELPER").is_ok_and(|v| v == "1");
    let mut out = Vec::new();
    for entry in entries {
        let claimed: Result<DynHostUsb, String> = if via_helper {
            HelperHostUsb::claim(entry.vendor_id, entry.product_id)
                .map(|b| Box::new(b) as DynHostUsb)
                .map_err(|err| err.to_string())
        } else {
            claim_by_vid_pid(entry.vendor_id, entry.product_id)
                .map(|b| Box::new(b) as DynHostUsb)
                .map_err(|err| err.to_string())
        };
        match claimed {
            Ok(boxed) => {
                tracing::info!(
                    target: "ternvale::usb",
                    vendor_id = format!("{:#06x}", entry.vendor_id),
                    product_id = format!("{:#06x}", entry.product_id),
                    port = entry.port,
                    via_helper,
                    "host usb device claimed for passthrough"
                );
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
