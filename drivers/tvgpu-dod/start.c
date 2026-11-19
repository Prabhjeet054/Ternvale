#include "include/tvgpu.h"
#include "include/trace.h"
#include "start.tmh"

NTSTATUS
TvgpuAddDevice(_In_ DEVICE_OBJECT *PhysicalDeviceObject, _Outptr_ PVOID *MiniportDeviceContext)
{
    TVGPU_DEVICE *dev;

    if (MiniportDeviceContext == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *MiniportDeviceContext = NULL;
    dev = (TVGPU_DEVICE *)ExAllocatePool2(POOL_FLAG_NON_PAGED, sizeof(*dev), TVGPU_POOL_TAG);
    if (dev == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(dev, sizeof(*dev));
    dev->PhysicalDeviceObject = PhysicalDeviceObject;
    dev->NextResourceId = 1;
    KeInitializeEvent(&dev->ControlDone, NotificationEvent, FALSE);
    KeInitializeEvent(&dev->CursorDone, NotificationEvent, FALSE);
    KeInitializeMutex(&dev->CmdMutex, 0);
    KeInitializeSpinLock(&dev->Lock);
    *MiniportDeviceContext = dev;
    TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DRIVER, "AddDevice");
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuStartDevice(
    _In_ PVOID MiniportDeviceContext,
    _In_ PDXGK_START_INFO DxgkStartInfo,
    _In_ PDXGKRNL_INTERFACE DxgkInterface,
    _Out_ PULONG NumberOfVideoPresentSources,
    _Out_ PULONG NumberOfChildren)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    DXGK_DEVICE_INFO info;
    NTSTATUS status;
    UNREFERENCED_PARAMETER(DxgkStartInfo);

    if (dev == NULL || DxgkInterface == NULL || NumberOfVideoPresentSources == NULL ||
        NumberOfChildren == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    RtlCopyMemory(&dev->Dxgk, DxgkInterface, sizeof(dev->Dxgk));
    RtlZeroMemory(&info, sizeof(info));
    status = DxgkInterface->DxgkCbGetDeviceInformation(DxgkInterface->DeviceHandle, &info);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = TvgpuMapBars(dev, info.TranslatedResourceList);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (dev->PhysicalDeviceObject != NULL) {
        dev->ConfigWork = IoAllocateWorkItem(dev->PhysicalDeviceObject);
    }
    status = TvgpuVirtioStart(dev);
    if (!NT_SUCCESS(status)) {
        TvgpuVirtioStop(dev);
        return status;
    }
    status = TvgpuRefreshDisplays(dev);
    if (!NT_SUCCESS(status)) {
        TraceEvents(TRACE_LEVEL_WARNING, TRACE_DEVICE, "GET_DISPLAY_INFO failed at start");
    }
    *NumberOfVideoPresentSources = dev->NumScanouts;
    *NumberOfChildren = dev->NumScanouts;
    TvgpuAppendLog("StartDevice");
    TraceEvents(
        TRACE_LEVEL_INFORMATION,
        TRACE_DRIVER,
        "StartDevice scanouts=%u edid=%u cursorq=%u",
        dev->NumScanouts,
        dev->EdidOffered,
        dev->CursorqReady);
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuStopDevice(_In_ PVOID MiniportDeviceContext)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    UINT32 i;

    if (dev == NULL) {
        return STATUS_SUCCESS;
    }
    for (i = 0; i < dev->NumScanouts && i < VIRTIO_GPU_MAX_SCANOUTS; i++) {
        if (dev->Scanouts[i].ResourceId != 0) {
            (VOID)TvgpuDisableScanout(dev, i);
        }
    }
    if (dev->ConfigWork != NULL) {
        IoFreeWorkItem(dev->ConfigWork);
        dev->ConfigWork = NULL;
    }
    TvgpuVirtioStop(dev);
    TvgpuAppendLog("StopDevice");
    TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DRIVER, "StopDevice");
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuRemoveDevice(_In_ PVOID MiniportDeviceContext)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    if (dev != NULL) {
        ExFreePoolWithTag(dev, TVGPU_POOL_TAG);
    }
    TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DRIVER, "RemoveDevice");
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuDispatchIoRequest(
    _In_ PVOID MiniportDeviceContext,
    _In_ ULONG VidPnSourceId,
    _In_ PVIDEO_REQUEST_PACKET VideoRequestPacket)
{
    UNREFERENCED_PARAMETER(MiniportDeviceContext);
    UNREFERENCED_PARAMETER(VidPnSourceId);
    if (VideoRequestPacket != NULL) {
        VideoRequestPacket->StatusBlock->Status = STATUS_NOT_IMPLEMENTED;
    }
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuStopDeviceAndReleasePostDisplayOwnership(
    _In_ PVOID MiniportDeviceContext,
    _In_ D3DDDI_VIDEO_PRESENT_TARGET_ID TargetId,
    _Out_ PDXGK_DISPLAY_INFORMATION DisplayInfo)
{
    UNREFERENCED_PARAMETER(TargetId);
    if (DisplayInfo != NULL) {
        RtlZeroMemory(DisplayInfo, sizeof(*DisplayInfo));
        DisplayInfo->Width = 1280;
        DisplayInfo->Height = 800;
        DisplayInfo->Pitch = 1280 * 4;
        DisplayInfo->ColorFormat = D3DDDIFMT_A8R8G8B8;
    }
    return TvgpuStopDevice(MiniportDeviceContext);
}
