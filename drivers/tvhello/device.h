#pragma once

#include <ntddk.h>
#include <wdf.h>

#include "public.h"

#define TVHELLO_POOL_TAG '0hvt' /* 'tvh0' */
#define TVHELLO_VERSION 1

typedef struct _TVHELLO_CONTEXT {
    ULONG Magic;
    ULONG D0Count;
} TVHELLO_CONTEXT, *PTVHELLO_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(TVHELLO_CONTEXT, TvHelloGetContext)

DRIVER_INITIALIZE DriverEntry;
EVT_WDF_DRIVER_DEVICE_ADD TvHelloEvtDeviceAdd;
EVT_WDF_OBJECT_CONTEXT_CLEANUP TvHelloEvtDriverContextCleanup;
EVT_WDF_DEVICE_D0_ENTRY TvHelloEvtD0Entry;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL TvHelloEvtIoDeviceControl;

NTSTATUS
TvHelloAppendLog(_In_ PCSTR Message);
