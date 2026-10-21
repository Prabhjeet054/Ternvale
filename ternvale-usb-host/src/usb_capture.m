// IOUSBHost capture + synchronous control/bulk transfers for Ternvale passthrough.
#import <Foundation/Foundation.h>
#import <IOKit/IOKitLib.h>
#import <IOUSBHost/IOUSBHost.h>
#import "ternvale_usb_capture.h"

struct TernvaleUsbDevice {
    IOUSBHostDevice *device;
    TernvaleUsbDisconnectFn on_disconnect;
    void *disconnect_ctx;
};

static IOUSBHostInterface *copy_interface_for_endpoint(IOUSBHostDevice *device, uint8_t endpoint_address,
                                                     NSError **error) {
    io_service_t service = [device ioService];
    io_iterator_t iter = 0;
    kern_return_t kr = IORegistryEntryGetChildIterator(service, kIOServicePlane, &iter);
    if (kr != KERN_SUCCESS) {
        if (error) {
            *error = [NSError errorWithDomain:NSPOSIXErrorDomain code:kr
                                     userInfo:@{NSLocalizedDescriptionKey : @"IORegistryEntryGetChildIterator"}];
        }
        return nil;
    }
    IOUSBHostInterface *found = nil;
    for (io_service_t child = IOIteratorNext(iter); child != 0; child = IOIteratorNext(iter)) {
        IOUSBHostInterface *iface =
            [[IOUSBHostInterface alloc] initWithIOService:child
                                                options:IOUSBHostObjectInitOptionsNone
                                                  queue:NULL
                                                  error:error
                                        interestHandler:nil];
        IOObjectRelease(child);
        if (!iface) {
            continue;
        }
        IOUSBHostPipe *pipe = [iface copyPipeWithAddress:endpoint_address error:error];
        if (pipe) {
            found = iface;
            break;
        }
        [iface destroy];
    }
    IOObjectRelease(iter);
    return found;
}

TernvaleUsbDevice *ternvale_usb_device_open(io_service_t service, TernvaleUsbDisconnectFn on_disconnect,
                                            void *disconnect_ctx, int32_t *ior_out) {
    if (ior_out) {
        *ior_out = 0;
    }
    kern_return_t auth = IOServiceAuthorize(service, kIOServiceInteractionAllowed);
    if (auth != KERN_SUCCESS && auth != kIOReturnNotPermitted) {
        if (ior_out) {
            *ior_out = auth;
        }
        return NULL;
    }

    struct TernvaleUsbDevice *dev = calloc(1, sizeof(struct TernvaleUsbDevice));
    if (!dev) {
        if (ior_out) {
            *ior_out = KERN_NO_SPACE;
        }
        return NULL;
    }
    dev->on_disconnect = on_disconnect;
    dev->disconnect_ctx = disconnect_ctx;
    __block struct TernvaleUsbDevice *block_dev = dev;

    NSError *error = nil;
    IOUSBHostDevice *host =
        [[IOUSBHostDevice alloc] initWithIOService:service
                                           options:IOUSBHostObjectInitOptionsDeviceCapture |
                                                   IOUSBHostObjectInitOptionsDeviceSeize
                                             queue:NULL
                                             error:&error
                                   interestHandler:^(__unused IOUSBHostObject *obj, uint32_t messageType,
                                                     __unused void *messageArgument) {
                                       if (messageType == kIOMessageServiceIsTerminated && block_dev &&
                                           block_dev->on_disconnect) {
                                           block_dev->on_disconnect(block_dev->disconnect_ctx);
                                       }
                                   }];
    if (!host) {
        free(dev);
        if (ior_out && error) {
            *ior_out = (int32_t)error.code;
        } else if (ior_out) {
            *ior_out = -1;
        }
        return NULL;
    }
    dev->device = host;
    return dev;
}

void ternvale_usb_device_close(TernvaleUsbDevice *dev) {
    if (!dev) {
        return;
    }
    if (dev->device) {
        [dev->device destroy];
        dev->device = nil;
    }
    free(dev);
}

static IOUSBDeviceRequest make_request(TernvaleUsbSetup setup) {
    IOUSBDeviceRequest req;
    req.bmRequestType = setup.bm_request_type;
    req.bRequest = setup.b_request;
    req.wValue = setup.w_value;
    req.wIndex = setup.w_index;
    req.wLength = setup.w_length;
    return req;
}

int32_t ternvale_usb_device_control(TernvaleUsbDevice *dev, TernvaleUsbSetup setup, const uint8_t *data_out,
                                    uint32_t data_out_len, uint8_t *data_in, uint32_t data_in_cap,
                                    uint32_t *transferred) {
    if (!dev || !dev->device) {
        return kIOReturnNotAttached;
    }
    IOUSBDeviceRequest req = make_request(setup);
    NSMutableData *data = nil;
    BOOL is_in = (setup.bm_request_type & 0x80) != 0;
    if (is_in && data_in_cap > 0) {
        data = [NSMutableData dataWithLength:data_in_cap];
    } else if (!is_in && data_out && data_out_len > 0) {
        data = [NSMutableData dataWithBytes:data_out length:data_out_len];
    }
    NSUInteger bytes = 0;
    NSError *error = nil;
    BOOL ok = [dev->device sendDeviceRequest:req
                                        data:data
                            bytesTransferred:&bytes
                           completionTimeout:5.0
                                       error:&error];
    if (!ok) {
        return error ? (int32_t)error.code : kIOReturnIOError;
    }
    if (is_in && data && data_in && bytes > 0) {
        NSUInteger copy = MIN(bytes, data_in_cap);
        memcpy(data_in, data.mutableBytes, copy);
        if (transferred) {
            *transferred = (uint32_t)copy;
        }
    } else if (transferred) {
        *transferred = (uint32_t)bytes;
    }
    return KERN_SUCCESS;
}

int32_t ternvale_usb_device_pipe_io(TernvaleUsbDevice *dev, uint8_t endpoint_address, const uint8_t *data_out,
                                    uint32_t data_out_len, uint8_t *data_in, uint32_t data_in_cap,
                                    uint32_t *transferred) {
    if (!dev || !dev->device) {
        return kIOReturnNotAttached;
    }
    NSError *error = nil;
    IOUSBHostInterface *iface = copy_interface_for_endpoint(dev->device, endpoint_address, &error);
    if (!iface) {
        return error ? (int32_t)error.code : kIOReturnNotFound;
    }
    IOUSBHostPipe *pipe = [iface copyPipeWithAddress:endpoint_address error:&error];
    if (!pipe) {
        [iface destroy];
        return error ? (int32_t)error.code : kIOReturnNotFound;
    }
    BOOL is_in = (endpoint_address & 0x80) != 0;
    NSMutableData *data = nil;
    if (is_in && data_in_cap > 0) {
        data = [NSMutableData dataWithLength:data_in_cap];
    } else if (!is_in && data_out && data_out_len > 0) {
        data = [NSMutableData dataWithBytes:data_out length:data_out_len];
    }
    NSUInteger bytes = 0;
    NSTimeInterval timeout = is_in ? 5.0 : 5.0;
    BOOL ok = [pipe sendIORequestWithData:data
                       bytesTransferred:&bytes
                      completionTimeout:timeout
                                  error:&error];
    [iface destroy];
    if (!ok) {
        return error ? (int32_t)error.code : kIOReturnIOError;
    }
    if (is_in && data && data_in && bytes > 0) {
        NSUInteger copy = MIN(bytes, data_in_cap);
        memcpy(data_in, data.mutableBytes, copy);
        if (transferred) {
            *transferred = (uint32_t)copy;
        }
    } else if (transferred) {
        *transferred = is_in ? (uint32_t)bytes : data_out_len;
    }
    return KERN_SUCCESS;
}
