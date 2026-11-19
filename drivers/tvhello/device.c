#include "device.h"
#include "trace.h"
#include "device.tmh"

#ifdef ALLOC_PRAGMA
#pragma alloc_text(PAGE, TvHelloEvtDeviceAdd)
#pragma alloc_text(PAGE, TvHelloEvtD0Entry)
#pragma alloc_text(PAGE, TvHelloEvtIoDeviceControl)
#endif

NTSTATUS
TvHelloEvtDeviceAdd(_In_ WDFDRIVER Driver, _Inout_ PWDFDEVICE_INIT DeviceInit)
{
    NTSTATUS status;
    WDFDEVICE device;
    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_PNPPOWER_EVENT_CALLBACKS pnp;
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDFQUEUE queue;
    PTVHELLO_CONTEXT ctx;

    UNREFERENCED_PARAMETER(Driver);
    PAGED_CODE();

    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnp);
    pnp.EvtDeviceD0Entry = TvHelloEvtD0Entry;
    WdfDeviceInitSetPnpPowerEventCallbacks(DeviceInit, &pnp);

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, TVHELLO_CONTEXT);
    status = WdfDeviceCreate(&DeviceInit, &attributes, &device);
    if (!NT_SUCCESS(status)) {
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_DEVICE, "WdfDeviceCreate failed %!STATUS!", status);
        return status;
    }

    ctx = TvHelloGetContext(device);
    ctx->Magic = TVHELLO_QUERY_MAGIC;
    ctx->D0Count = 0;

    status = WdfDeviceCreateDeviceInterface(device, &GUID_DEVINTERFACE_TVHELLO, NULL);
    if (!NT_SUCCESS(status)) {
        TraceEvents(
            TRACE_LEVEL_ERROR,
            TRACE_DEVICE,
            "WdfDeviceCreateDeviceInterface failed %!STATUS!",
            status);
        return status;
    }

    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchSequential);
    queueConfig.EvtIoDeviceControl = TvHelloEvtIoDeviceControl;
    status = WdfIoQueueCreate(device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, &queue);
    if (!NT_SUCCESS(status)) {
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_DEVICE, "WdfIoQueueCreate failed %!STATUS!", status);
        return status;
    }

    TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DEVICE, "tvhello device interface registered");
    (void)TvHelloAppendLog("tvhello: device add, interface GUID_DEVINTERFACE_TVHELLO");
    return STATUS_SUCCESS;
}

NTSTATUS
TvHelloEvtD0Entry(_In_ WDFDEVICE Device, _In_ WDF_POWER_DEVICE_STATE PreviousState)
{
    PTVHELLO_CONTEXT ctx;

    PAGED_CODE();
    ctx = TvHelloGetContext(Device);
    ctx->D0Count += 1;

    TraceEvents(
        TRACE_LEVEL_INFORMATION,
        TRACE_DEVICE,
        "tvhello D0 entry previous=%d count=%lu",
        (int)PreviousState,
        ctx->D0Count);
    (void)TvHelloAppendLog("tvhello: D0 entry");
    return STATUS_SUCCESS;
}

VOID
TvHelloEvtIoDeviceControl(
    _In_ WDFQUEUE Queue,
    _In_ WDFREQUEST Request,
    _In_ size_t OutputBufferLength,
    _In_ size_t InputBufferLength,
    _In_ ULONG IoControlCode)
{
    NTSTATUS status = STATUS_INVALID_DEVICE_REQUEST;
    WDFDEVICE device;
    PTVHELLO_CONTEXT ctx;
    PTVHELLO_QUERY query = NULL;
    PVOID buffer = NULL;
    size_t length = 0;

    UNREFERENCED_PARAMETER(InputBufferLength);
    PAGED_CODE();

    device = WdfIoQueueGetDevice(Queue);
    ctx = TvHelloGetContext(device);

    switch (IoControlCode) {
    case IOCTL_TVHELLO_QUERY:
        if (OutputBufferLength < sizeof(TVHELLO_QUERY)) {
            status = STATUS_BUFFER_TOO_SMALL;
            break;
        }
        status = WdfRequestRetrieveOutputBuffer(
            Request,
            sizeof(TVHELLO_QUERY),
            &buffer,
            &length);
        if (!NT_SUCCESS(status)) {
            break;
        }
        query = (PTVHELLO_QUERY)buffer;
        query->Magic = ctx->Magic;
        query->Version = TVHELLO_VERSION;
        query->D0Count = ctx->D0Count;
        WdfRequestSetInformation(Request, sizeof(TVHELLO_QUERY));
        (void)TvHelloAppendLog("tvhello: IOCTL_TVHELLO_QUERY");
        TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DEVICE, "tvhello query d0=%lu", ctx->D0Count);
        break;

    case IOCTL_TVHELLO_BUGCHECK:
#ifdef TVHELLO_SCRATCH_BUGCHECK
        /* Scratch builds only (drivers/build.ps1 -ScratchBugcheck). */
        (void)TvHelloAppendLog("tvhello: IOCTL_TVHELLO_BUGCHECK (scratch)");
        KeBugCheckEx(MANUALLY_INITIATED_CRASH, (ULONG_PTR)'0hvt', 0, 0, 0);
#else
        status = STATUS_NOT_SUPPORTED;
        (void)TvHelloAppendLog("tvhello: IOCTL_TVHELLO_BUGCHECK refused (not scratch)");
#endif
        break;

    default:
        status = STATUS_INVALID_DEVICE_REQUEST;
        break;
    }

    WdfRequestComplete(Request, status);
}
