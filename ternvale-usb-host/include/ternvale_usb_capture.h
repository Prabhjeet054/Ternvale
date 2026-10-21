// C ABI for IOUSBHost device capture and transfers (implemented in usb_capture.m).
#pragma once

#include <IOKit/IOTypes.h>
#include <stdint.h>

typedef struct TernvaleUsbSetup {
    uint8_t bm_request_type;
    uint8_t b_request;
    uint16_t w_value;
    uint16_t w_index;
    uint16_t w_length;
} TernvaleUsbSetup;

typedef struct TernvaleUsbDevice TernvaleUsbDevice;

typedef void (*TernvaleUsbDisconnectFn)(void *ctx);

/// Open with `IOUSBHostObjectInitOptionsDeviceCapture | DeviceSeize` after `IOServiceAuthorize`.
/// Returns NULL on failure; `*ior_out` holds IOReturn when non-NULL.
TernvaleUsbDevice *ternvale_usb_device_open(io_service_t service, TernvaleUsbDisconnectFn on_disconnect,
                                            void *disconnect_ctx, int32_t *ior_out);

void ternvale_usb_device_close(TernvaleUsbDevice *dev);

/// Synchronous control transfer on the default pipe. `transferred` may be NULL.
int32_t ternvale_usb_device_control(TernvaleUsbDevice *dev, TernvaleUsbSetup setup,
                                    const uint8_t *data_out, uint32_t data_out_len, uint8_t *data_in,
                                    uint32_t data_in_cap, uint32_t *transferred);

/// Bulk or interrupt IO on `endpoint_address` (e.g. `0x81`). Direction inferred from bit 7.
int32_t ternvale_usb_device_pipe_io(TernvaleUsbDevice *dev, uint8_t endpoint_address,
                                    const uint8_t *data_out, uint32_t data_out_len, uint8_t *data_in,
                                    uint32_t data_in_cap, uint32_t *transferred);
