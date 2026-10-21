//! Raw bindings to `usb_capture.m` (IOUSBHost).

use std::os::raw::c_void;

use io_kit_sys::types::io_service_t;

#[repr(C)]
pub struct TernvaleUsbSetup {
    pub bm_request_type: u8,
    pub b_request: u8,
    pub w_value: u16,
    pub w_index: u16,
    pub w_length: u16,
}

pub type TernvaleUsbDevice = c_void;

pub type TernvaleUsbDisconnectFn = Option<extern "C" fn(*mut c_void)>;

extern "C" {
    pub fn ternvale_usb_device_open(
        service: io_service_t,
        on_disconnect: TernvaleUsbDisconnectFn,
        disconnect_ctx: *mut c_void,
        ior_out: *mut i32,
    ) -> *mut TernvaleUsbDevice;

    pub fn ternvale_usb_device_close(dev: *mut TernvaleUsbDevice);

    pub fn ternvale_usb_device_control(
        dev: *mut TernvaleUsbDevice,
        setup: TernvaleUsbSetup,
        data_out: *const u8,
        data_out_len: u32,
        data_in: *mut u8,
        data_in_cap: u32,
        transferred: *mut u32,
    ) -> i32;

    pub fn ternvale_usb_device_pipe_io(
        dev: *mut TernvaleUsbDevice,
        endpoint_address: u8,
        data_out: *const u8,
        data_out_len: u32,
        data_in: *mut u8,
        data_in_cap: u32,
        transferred: *mut u32,
    ) -> i32;
}
