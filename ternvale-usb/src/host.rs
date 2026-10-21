//! Host USB backend trait (implemented in `ternvale-usb-host` on macOS).

use std::fmt;

/// Standard 8-byte USB control setup packet (little-endian fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbControlSetup {
    /// `bmRequestType`
    pub request_type: u8,
    /// `bRequest`
    pub request: u8,
    /// `wValue`
    pub value: u16,
    /// `wIndex`
    pub index: u16,
    /// `wLength`
    pub length: u16,
}

impl UsbControlSetup {
    /// Parse a guest setup packet.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 {
            return None;
        }
        Some(Self {
            request_type: bytes[0],
            request: bytes[1],
            value: u16::from_le_bytes([bytes[2], bytes[3]]),
            index: u16::from_le_bytes([bytes[4], bytes[5]]),
            length: u16::from_le_bytes([bytes[6], bytes[7]]),
        })
    }

    /// Wire format for logging and host APIs.
    #[must_use]
    pub fn to_bytes(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0] = self.request_type;
        out[1] = self.request;
        out[2..4].copy_from_slice(&self.value.to_le_bytes());
        out[4..6].copy_from_slice(&self.index.to_le_bytes());
        out[6..8].copy_from_slice(&self.length.to_le_bytes());
        out
    }
}

/// Host I/O failure surfaced to the guest as a stalled transfer.
#[derive(Debug, thiserror::Error)]
pub enum HostUsbError {
    /// Device is not open or was unplugged.
    #[error("host usb device unavailable: {message}")]
    Unavailable {
        /// Detail for logs.
        message: String,
    },
    /// Kernel or driver rejected the request.
    #[error("host usb transfer failed: {message} (code {code:#x})")]
    Transfer {
        /// IOKit / errno style code.
        code: i32,
        /// Human-readable detail.
        message: String,
    },
    /// Argument or buffer sizing error.
    #[error("host usb invalid request: {0}")]
    Invalid(String),
}

/// A claimed host USB device servicing guest transfers on a worker thread.
pub trait HostUsbBackend: Send + fmt::Debug {
    /// USB vendor ID of the opened device.
    fn vendor_id(&self) -> u16;
    /// USB product ID of the opened device.
    fn product_id(&self) -> u16;
    /// Short label for logs (registry path or service name).
    fn label(&self) -> &str;

    /// Synchronous control transfer (runs on the passthrough worker thread only).
    ///
    /// For IN requests (`request_type & 0x80 != 0`), fills `data_in` and returns bytes written.
    /// For OUT, sends `data_out` prefix.
    fn control_transfer(
        &self,
        setup: UsbControlSetup,
        data_out: Option<&[u8]>,
        data_in: &mut [u8],
    ) -> Result<usize, HostUsbError>;

    /// Bulk or interrupt transfer on a configured endpoint address.
    fn pipe_transfer(
        &self,
        endpoint_address: u8,
        data_out: Option<&[u8]>,
        data_in: &mut [u8],
    ) -> Result<usize, HostUsbError> {
        let _ = (endpoint_address, data_out, data_in);
        Err(HostUsbError::Unavailable {
            message: "pipe_transfer not implemented for this backend".into(),
        })
    }

    /// Called once the passthrough worker knows the guest root port (opens host capture).
    fn wire_disconnect_notifier(&mut self, _port: u8, _tx: std::sync::mpsc::Sender<u8>) {}

    /// Mark the device gone after an unplug notification from the host.
    fn on_host_disconnect(&self) {}
}
