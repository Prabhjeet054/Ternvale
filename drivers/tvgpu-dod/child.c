#include "include/tvgpu.h"
#include "include/trace.h"
#include "child.tmh"

NTSTATUS
TvgpuQueryChildRelations(
    _In_ PVOID MiniportDeviceContext,
    _Out_writes_bytes_(ChildRelationsSize) PDXGK_CHILD_DESCRIPTOR ChildRelations,
    _In_ ULONG ChildRelationsSize)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    ULONG i;
    ULONG need;

    if (dev == NULL || ChildRelations == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    need = (dev->NumScanouts + 1) * sizeof(DXGK_CHILD_DESCRIPTOR);
    if (ChildRelationsSize < need) {
        return STATUS_BUFFER_TOO_SMALL;
    }
    RtlZeroMemory(ChildRelations, ChildRelationsSize);
    for (i = 0; i < dev->NumScanouts; i++) {
        ChildRelations[i].ChildDeviceType = TypeVideoOutput;
        ChildRelations[i].ChildCapabilities.HpdAwareness = HpdAwarenessInterruptible;
        ChildRelations[i].ChildCapabilities.Type.VideoOutput.InterfaceTechnology =
            D3DKMDT_VOT_HD15;
        ChildRelations[i].ChildCapabilities.Type.VideoOutput.MonitorOrientationAwareness =
            D3DKMDT_MOA_NONE;
        ChildRelations[i].ChildCapabilities.Type.VideoOutput.SupportsSdtvModes = FALSE;
        ChildRelations[i].AcpiUid = i;
        ChildRelations[i].ChildUid = i;
    }
    TraceEvents(
        TRACE_LEVEL_INFORMATION,
        TRACE_DEVICE,
        "child relations %u",
        dev->NumScanouts);
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuQueryChildStatus(
    _In_ PVOID MiniportDeviceContext,
    _Inout_ PDXGK_CHILD_STATUS ChildStatus,
    _In_ BOOLEAN NonDestructiveOnly)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    UNREFERENCED_PARAMETER(NonDestructiveOnly);

    if (dev == NULL || ChildStatus == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!NT_SUCCESS(TvgpuValidateScanoutId(dev, ChildStatus->ChildUid))) {
        return STATUS_GRAPHICS_INVALID_CHILD_UID;
    }
    if (ChildStatus->Type == StatusConnection) {
        ChildStatus->HotPlug.Connected = dev->Scanouts[ChildStatus->ChildUid].Connected;
    }
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuQueryDeviceDescriptor(
    _In_ PVOID MiniportDeviceContext,
    _In_ ULONG ChildUid,
    _Inout_ PDXGK_DEVICE_DESCRIPTOR DeviceDescriptor)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    UINT32 copy;

    if (dev == NULL || DeviceDescriptor == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!NT_SUCCESS(TvgpuValidateScanoutId(dev, ChildUid))) {
        return STATUS_GRAPHICS_INVALID_CHILD_UID;
    }
    if (!dev->Scanouts[ChildUid].HasEdid) {
        return STATUS_MONITOR_NO_DESCRIPTOR;
    }
    if (DeviceDescriptor->DescriptorOffset >= TVGPU_EDID_MIN) {
        return STATUS_MONITOR_NO_MORE_DESCRIPTOR_DATA;
    }
    copy = TVGPU_EDID_MIN - DeviceDescriptor->DescriptorOffset;
    if (copy > DeviceDescriptor->DescriptorLength) {
        copy = DeviceDescriptor->DescriptorLength;
    }
    RtlCopyMemory(
        DeviceDescriptor->DescriptorBuffer,
        dev->Scanouts[ChildUid].Edid + DeviceDescriptor->DescriptorOffset,
        copy);
    DeviceDescriptor->DescriptorLength = copy;
    return STATUS_SUCCESS;
}
