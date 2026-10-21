//! macOS IOKit USB device claim and control transfers.
//!
//! See `docs/USB_PASSTHROUGH.md` for permissions and limitations.

use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Mutex};

use core_foundation::base::TCFType;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::kCFAllocatorDefault;
use io_kit_sys::keys::kIOServicePlane;
use io_kit_sys::types::{io_iterator_t, io_service_t};
use io_kit_sys::{
    kIOMasterPortDefault, IOIteratorNext, IOObjectRelease, IORegistryEntryCopyPath,
    IORegistryEntryCreateCFProperty, IOServiceGetMatchingServices, IOServiceMatching,
};
use mach2::kern_return::KERN_SUCCESS;

/// `kIOReturnNotAttached` from IOKit.
const IO_RETURN_NOT_ATTACHED: i32 = 0xe000_02e4_u32 as i32;

use ternvale_usb::{HostUsbBackend, HostUsbError, UsbControlSetup};

use crate::ffi::{self, TernvaleUsbSetup};

/// macOS-specific claim / transfer error.
#[derive(Debug, thiserror::Error)]
pub enum MacosUsbError {
    /// IOKit returned an error.
    #[error("iokit error {code:#x}: {message}")]
    Io {
        /// `kern_return_t` or `IOReturn`.
        code: i32,
        /// Context for logs.
        message: String,
    },
    /// No matching device on the bus.
    #[error("no usb device {vendor_id:#06x}:{product_id:#06x} on the host")]
    NotFound {
        /// Vendor ID from config.
        vendor_id: u16,
        /// Product ID from config.
        product_id: u16,
    },
}

struct DisconnectCtx {
    port: u8,
    tx: Sender<u8>,
}

/// Owned IOUSBHost handle (only touched on the passthrough worker thread).
struct CapturedDevice(*mut ffi::TernvaleUsbDevice);

// SAFETY: The passthrough worker is the only Rust code that calls into IOUSBHost for this handle.
unsafe impl Send for CapturedDevice {}

/// A host USB device opened through IOUSBHost for passthrough.
pub struct MacosHostUsb {
    vendor_id: u16,
    product_id: u16,
    label: String,
    service: io_service_t,
    handle: Mutex<CapturedDevice>,
    disconnected: AtomicBool,
    disconnect_ctx: Mutex<Option<Box<DisconnectCtx>>>,
}

impl Drop for MacosHostUsb {
    fn drop(&mut self) {
        let mut guard = self.handle.lock().expect("usb handle lock");
        if !guard.0.is_null() {
            // SAFETY: pointer from `ternvale_usb_device_open`.
            unsafe {
                ffi::ternvale_usb_device_close(guard.0);
            }
            guard.0 = std::ptr::null_mut();
        }
        if self.service != 0 {
            // SAFETY: `service` was retained from `IOIteratorNext`.
            unsafe {
                IOObjectRelease(self.service);
            }
            self.service = 0;
        }
    }
}

impl std::fmt::Debug for MacosHostUsb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacosHostUsb")
            .field("vendor_id", &self.vendor_id)
            .field("product_id", &self.product_id)
            .field("label", &self.label)
            .field("disconnected", &self.disconnected.load(Ordering::Acquire))
            .finish()
    }
}

extern "C" fn disconnect_callback(ctx: *mut c_void) {
    // SAFETY: `ctx` points at `DisconnectCtx` owned by `MacosHostUsb`.
    let ctx = unsafe { &*(ctx as *const DisconnectCtx) };
    tracing::warn!(
        target: "ternvale::usb",
        port = ctx.port,
        "host usb device disconnected (service terminated)"
    );
    if let Err(err) = ctx.tx.send(ctx.port) {
        tracing::warn!(
            target: "ternvale::usb",
            port = ctx.port,
            error = %err,
            "failed to notify passthrough worker of usb disconnect"
        );
    }
}

/// Locate and open a USB device by vendor/product ID with capture/seize.
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all, fields(vendor_id = format!("{vendor_id:#06x}"), product_id = format!("{product_id:#06x}")))]
pub fn claim_by_vid_pid(vendor_id: u16, product_id: u16) -> Result<MacosHostUsb, MacosUsbError> {
    let service = find_service(vendor_id, product_id)?;
    let label =
        registry_path(service).unwrap_or_else(|| format!("{vendor_id:#06x}:{product_id:#06x}"));
    tracing::info!(
        target: "ternvale::usb",
        path = %label,
        "host usb device matched; capture opens when passthrough worker starts"
    );
    Ok(MacosHostUsb {
        vendor_id,
        product_id,
        label,
        service,
        handle: Mutex::new(CapturedDevice(std::ptr::null_mut())),
        disconnected: AtomicBool::new(false),
        disconnect_ctx: Mutex::new(None),
    })
}

impl MacosHostUsb {
    /// Register guest root port and worker channel for unplug notifications.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip(self, tx))]
    pub fn wire_disconnect_notifier(&mut self, port: u8, tx: Sender<u8>) {
        let ctx = Box::new(DisconnectCtx { port, tx });
        let ctx_ptr = Box::into_raw(ctx);
        let mut ior = 0i32;
        // SAFETY: `service` retained; `ctx_ptr` lives until `MacosHostUsb` drops.
        let handle = unsafe {
            ffi::ternvale_usb_device_open(
                self.service,
                Some(disconnect_callback),
                ctx_ptr as *mut c_void,
                &mut ior,
            )
        };
        if handle.is_null() {
            tracing::error!(
                target: "ternvale::usb",
                port,
                code = ior,
                label = %self.label,
                "IOUSBHost device capture failed"
            );
            // SAFETY: reclaim boxed ctx on failure.
            unsafe {
                drop(Box::from_raw(ctx_ptr));
            }
            return;
        }
        tracing::info!(
            target: "ternvale::usb",
            port,
            label = %self.label,
            "host usb device captured for passthrough"
        );
        let mut guard = self.handle.lock().expect("usb handle lock");
        *guard = CapturedDevice(handle);
        let mut ctx_guard = self.disconnect_ctx.lock().expect("disconnect ctx lock");
        *ctx_guard = Some(unsafe { Box::from_raw(ctx_ptr) });
    }

    fn handle(&self) -> Result<*mut ffi::TernvaleUsbDevice, HostUsbError> {
        if self.disconnected.load(Ordering::Acquire) {
            return Err(HostUsbError::Unavailable {
                message: "host usb device unplugged".into(),
            });
        }
        let guard = self.handle.lock().map_err(|_| HostUsbError::Unavailable {
            message: "host usb handle lock poisoned".into(),
        })?;
        if guard.0.is_null() {
            return Err(HostUsbError::Unavailable {
                message: "host usb device not open".into(),
            });
        }
        Ok(guard.0)
    }

    fn mark_disconnected(&self) {
        self.disconnected.store(true, Ordering::Release);
    }
}

impl HostUsbBackend for MacosHostUsb {
    fn vendor_id(&self) -> u16 {
        self.vendor_id
    }

    fn product_id(&self) -> u16 {
        self.product_id
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn wire_disconnect_notifier(&mut self, port: u8, tx: std::sync::mpsc::Sender<u8>) {
        MacosHostUsb::wire_disconnect_notifier(self, port, tx);
    }

    fn control_transfer(
        &self,
        setup: UsbControlSetup,
        data_out: Option<&[u8]>,
        data_in: &mut [u8],
    ) -> Result<usize, HostUsbError> {
        let handle = self.handle()?;
        let req = TernvaleUsbSetup {
            bm_request_type: setup.request_type,
            b_request: setup.request,
            w_value: setup.value,
            w_index: setup.index,
            w_length: setup.length,
        };
        let (out_ptr, out_len) = match data_out {
            Some(slice) => (slice.as_ptr(), slice.len() as u32),
            None => (std::ptr::null(), 0),
        };
        let mut transferred = 0u32;
        tracing::trace!(
            target: "ternvale::usb",
            label = %self.label,
            request = setup.request,
            len = setup.length,
            "host IOUSBHost control transfer"
        );
        // SAFETY: `handle` is valid; buffers match lengths.
        let ior = unsafe {
            ffi::ternvale_usb_device_control(
                handle,
                req,
                out_ptr,
                out_len,
                data_in.as_mut_ptr(),
                data_in.len() as u32,
                &mut transferred,
            )
        };
        if ior == IO_RETURN_NOT_ATTACHED {
            self.mark_disconnected();
            return Err(HostUsbError::Unavailable {
                message: "host usb device detached during control transfer".into(),
            });
        }
        if ior != KERN_SUCCESS {
            tracing::warn!(
                target: "ternvale::usb",
                label = %self.label,
                code = ior,
                "host control transfer failed"
            );
            return Err(HostUsbError::Transfer {
                code: ior,
                message: "IOUSBHost sendDeviceRequest".into(),
            });
        }
        Ok(transferred as usize)
    }

    fn pipe_transfer(
        &self,
        endpoint_address: u8,
        data_out: Option<&[u8]>,
        data_in: &mut [u8],
    ) -> Result<usize, HostUsbError> {
        let handle = self.handle()?;
        let (out_ptr, out_len) = match data_out {
            Some(slice) => (slice.as_ptr(), slice.len() as u32),
            None => (std::ptr::null(), 0),
        };
        let mut transferred = 0u32;
        tracing::trace!(
            target: "ternvale::usb",
            label = %self.label,
            endpoint = format!("{endpoint_address:#04x}"),
            "host IOUSBHost pipe transfer"
        );
        // SAFETY: `handle` valid; guest buffers sized by caller.
        let ior = unsafe {
            ffi::ternvale_usb_device_pipe_io(
                handle,
                endpoint_address,
                out_ptr,
                out_len,
                data_in.as_mut_ptr(),
                data_in.len() as u32,
                &mut transferred,
            )
        };
        if ior == IO_RETURN_NOT_ATTACHED {
            self.mark_disconnected();
            return Err(HostUsbError::Unavailable {
                message: "host usb device detached during pipe transfer".into(),
            });
        }
        if ior != KERN_SUCCESS {
            tracing::warn!(
                target: "ternvale::usb",
                label = %self.label,
                endpoint = format!("{endpoint_address:#04x}"),
                code = ior,
                "host pipe transfer failed"
            );
            return Err(HostUsbError::Transfer {
                code: ior,
                message: "IOUSBHost pipe IO".into(),
            });
        }
        Ok(transferred as usize)
    }

    fn on_host_disconnect(&self) {
        self.mark_disconnected();
    }
}

fn find_service(vendor_id: u16, product_id: u16) -> Result<io_service_t, MacosUsbError> {
    let matching = unsafe { IOServiceMatching(c"IOUSBHostDevice".as_ptr()) };
    if matching.is_null() {
        return Err(MacosUsbError::Io {
            code: -1,
            message: "IOServiceMatching IOUSBHostDevice".into(),
        });
    }
    let mut iter: io_iterator_t = 0;
    let kr = unsafe { IOServiceGetMatchingServices(kIOMasterPortDefault, matching, &mut iter) };
    if kr != KERN_SUCCESS {
        return Err(MacosUsbError::Io {
            code: kr,
            message: "IOServiceGetMatchingServices".into(),
        });
    }
    loop {
        let service = unsafe { IOIteratorNext(iter) };
        if service == 0 {
            break;
        }
        let vid = registry_u16(service, "idVendor");
        let pid = registry_u16(service, "idProduct");
        if vid == Some(vendor_id) && pid == Some(product_id) {
            unsafe {
                IOObjectRelease(iter);
            }
            return Ok(service);
        }
        unsafe {
            IOObjectRelease(service);
        }
    }
    unsafe {
        IOObjectRelease(iter);
    }
    Err(MacosUsbError::NotFound {
        vendor_id,
        product_id,
    })
}

fn registry_u16(service: io_service_t, key: &str) -> Option<u16> {
    let key_cf = CFString::new(key);
    let value = unsafe {
        IORegistryEntryCreateCFProperty(
            service,
            key_cf.as_concrete_TypeRef() as *const _,
            kCFAllocatorDefault,
            0,
        )
    };
    if value.is_null() {
        return None;
    }
    let num = unsafe { CFNumber::wrap_under_create_rule(value as _) };
    num.to_i32().and_then(|v| u16::try_from(v).ok())
}

fn registry_path(service: io_service_t) -> Option<String> {
    let path = unsafe { IORegistryEntryCopyPath(service, kIOServicePlane) };
    if path.is_null() {
        return None;
    }
    let s = unsafe { CFString::wrap_under_create_rule(path as _) };
    Some(s.to_string())
}
