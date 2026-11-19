#include "include/tvgpu.h"
#include "include/trace.h"
#include "interrupt.tmh"

BOOLEAN
TvgpuInterruptRoutine(_In_ PVOID MiniportDeviceContext, _In_ ULONG MessageNumber)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    UINT8 isr;
    UNREFERENCED_PARAMETER(MessageNumber);

    if (dev == NULL || dev->Isr == NULL) {
        return FALSE;
    }
    isr = READ_REGISTER_UCHAR((volatile UINT8 *)dev->Isr);
    if (isr == 0) {
        return FALSE;
    }
    TraceEvents(TRACE_LEVEL_VERBOSE, TRACE_VIRTIO, "isr %#x", isr);
    if ((isr & VIRTIO_ISR_QUEUE) != 0) {
        KeSetEvent(&dev->ControlDone, IO_NO_INCREMENT, FALSE);
        KeSetEvent(&dev->CursorDone, IO_NO_INCREMENT, FALSE);
    }
    if ((isr & VIRTIO_ISR_CONFIG) != 0) {
        InterlockedOr(&dev->ConfigPending, 1);
    }
    return TRUE;
}

VOID
TvgpuDpcRoutine(_In_ PVOID MiniportDeviceContext)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;

    if (dev == NULL) {
        return;
    }
    if (InterlockedExchange(&dev->ConfigPending, 0) != 0 && dev->ConfigWork != NULL) {
        IoQueueWorkItem(dev->ConfigWork, TvgpuConfigWork, DelayedWorkQueue, dev);
    }
}

VOID
TvgpuConfigWork(_In_ DEVICE_OBJECT *DeviceObject, _In_opt_ PVOID Context)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)Context;
    UINT32 events;
    UINT32 i;
    DXGK_CHILD_STATUS child;
    UNREFERENCED_PARAMETER(DeviceObject);

    if (dev == NULL || dev->GpuCfg == NULL) {
        return;
    }
    events = READ_REGISTER_ULONG(&dev->GpuCfg->EventsRead);
    TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DEVICE, "config events %#x", events);
    if ((events & VIRTIO_GPU_EVENT_DISPLAY) != 0) {
        WRITE_REGISTER_ULONG(&dev->GpuCfg->EventsClear, VIRTIO_GPU_EVENT_DISPLAY);
        if (NT_SUCCESS(TvgpuRefreshDisplays(dev))) {
            for (i = 0; i < dev->NumScanouts && i < VIRTIO_GPU_MAX_SCANOUTS; i++) {
                RtlZeroMemory(&child, sizeof(child));
                child.Type = StatusConnection;
                child.ChildUid = i;
                child.HotPlug.Connected = dev->Scanouts[i].Connected ? TRUE : FALSE;
                (VOID)dev->Dxgk.DxgkCbIndicateChildStatus(dev->Dxgk.DeviceHandle, &child);
            }
            TvgpuAppendLog("display change indicated");
        }
    }
}
