//! Host-side injection into virtual HID devices (alternative to virtio-input).

use std::sync::{Arc, Mutex};

use crate::guest_mem::MemError;
use crate::hid::{BootKeyboardReport, BootMouseReport, HidKind};
use crate::Xhci;

/// Key/button injection for virtual USB HID devices.
#[derive(Debug, Default)]
pub struct UsbInputStats {
    /// Keyboard reports queued.
    pub keyboard: u64,
    /// Mouse reports queued.
    pub mouse: u64,
}

/// Thread-safe handle for injecting keyboard and pointer events.
pub struct UsbInput {
    xhci: Arc<Mutex<Xhci>>,
    stats: Arc<Mutex<UsbInputStats>>,
}

impl UsbInput {
    /// Wrap a controller the VMM already registered on the MMIO bus.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
    pub fn new(xhci: Arc<Mutex<Xhci>>) -> Self {
        tracing::info!(target: "ternvale::usb", "usb input sink ready (virtual hid)");
        Self {
            xhci,
            stats: Arc::new(Mutex::new(UsbInputStats::default())),
        }
    }

    /// Queue a boot keyboard report on the keyboard slot's interrupt IN endpoint.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip(self))]
    pub fn keyboard(&self, report: BootKeyboardReport) -> Result<(), InputError> {
        let mut guard = self.xhci.lock().map_err(|_| InputError::Poisoned)?;
        guard.queue_hid_report(HidKind::Keyboard, report.to_bytes().to_vec())?;
        self.stats
            .lock()
            .map_err(|_| InputError::Poisoned)?
            .keyboard += 1;
        tracing::trace!(target: "ternvale::usb", modifiers = report.modifiers, keys = ?report.keys, "usb keyboard report queued");
        Ok(())
    }

    /// Queue a boot mouse report.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip(self))]
    pub fn mouse(&self, report: BootMouseReport) -> Result<(), InputError> {
        let mut guard = self.xhci.lock().map_err(|_| InputError::Poisoned)?;
        guard.queue_hid_report(HidKind::Mouse, report.to_bytes().to_vec())?;
        self.stats.lock().map_err(|_| InputError::Poisoned)?.mouse += 1;
        tracing::trace!(
            target: "ternvale::usb",
            buttons = report.buttons,
            dx = report.dx,
            dy = report.dy,
            "usb mouse report queued"
        );
        Ok(())
    }

    /// Live counters.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
    pub fn stats(&self) -> UsbInputStats {
        self.stats
            .lock()
            .map(|s| UsbInputStats {
                keyboard: s.keyboard,
                mouse: s.mouse,
            })
            .unwrap_or_default()
    }
}

/// Injection failed.
#[derive(Debug, thiserror::Error)]
pub enum InputError {
    /// The controller mutex was poisoned.
    #[error("usb controller lock poisoned")]
    Poisoned,
    /// No slot matched the HID device.
    #[error("no xhci slot for {0:?}")]
    NoSlot(HidKind),
    /// Guest memory access failed while queueing a report.
    #[error(transparent)]
    Memory(#[from] MemError),
}
