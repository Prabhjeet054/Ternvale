//! VMM-side USB backend that talks to `ternvale-vmnetd` instead of IOKit.

use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;

use ternvale_net::socket_path;
use ternvale_usb::{HostUsbBackend, HostUsbError, UsbControlSetup};

use crate::helper_proto;

/// Host USB device claimed by the privileged helper.
pub struct HelperHostUsb {
    vendor_id: u16,
    product_id: u16,
    label: String,
    stream: Mutex<UnixStream>,
}

impl std::fmt::Debug for HelperHostUsb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperHostUsb")
            .field("vendor_id", &self.vendor_id)
            .field("product_id", &self.product_id)
            .field("label", &self.label)
            .finish()
    }
}

impl HelperHostUsb {
    /// Connect to the helper and claim `vendor_id`:`product_id`.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all, fields(vendor_id = format!("{vendor_id:#06x}"), product_id = format!("{product_id:#06x}")))]
    pub fn claim(vendor_id: u16, product_id: u16) -> Result<Self, HostUsbError> {
        let path = socket_path();
        let mut stream = UnixStream::connect(&path).map_err(|error| HostUsbError::Unavailable {
            message: format!("helper {}: {error}", path.display()),
        })?;
        helper_proto::write_claim(&mut stream, vendor_id, product_id).map_err(io_err)?;
        stream.flush().map_err(io_err)?;
        let (status, label) = helper_proto::read_ack(&mut stream).map_err(io_err)?;
        if status != 0 {
            tracing::warn!(
                target: "ternvale::usb",
                status,
                label = %label,
                "privileged helper usb claim failed"
            );
            return Err(HostUsbError::Unavailable { message: label });
        }
        tracing::info!(
            target: "ternvale::usb",
            path = %path.display(),
            label = %label,
            "usb device claimed via privileged helper"
        );
        Ok(Self {
            vendor_id,
            product_id,
            label,
            stream: Mutex::new(stream),
        })
    }
}

impl HostUsbBackend for HelperHostUsb {
    fn vendor_id(&self) -> u16 {
        self.vendor_id
    }

    fn product_id(&self) -> u16 {
        self.product_id
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn control_transfer(
        &self,
        setup: UsbControlSetup,
        data_out: Option<&[u8]>,
        data_in: &mut [u8],
    ) -> Result<usize, HostUsbError> {
        let mut stream = self.stream.lock().map_err(|_| HostUsbError::Unavailable {
            message: "usb helper lock poisoned".into(),
        })?;
        helper_proto::write_xfer(&mut *stream, setup, data_out, data_in.len() as u32)
            .map_err(io_err)?;
        stream.flush().map_err(io_err)?;
        let (status, bytes) = helper_proto::read_reply(&mut *stream).map_err(io_err)?;
        if status != 0 {
            return Err(HostUsbError::Transfer {
                code: status,
                message: "helper control transfer".into(),
            });
        }
        let n = bytes.len().min(data_in.len());
        data_in[..n].copy_from_slice(&bytes[..n]);
        Ok(n)
    }
}

fn io_err(error: io::Error) -> HostUsbError {
    HostUsbError::Unavailable {
        message: error.to_string(),
    }
}
