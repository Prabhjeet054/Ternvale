#include "include/tvgpu.h"
#include "include/trace.h"
#include "power.tmh"

NTSTATUS
TvgpuSetPowerState(
    _In_ PVOID MiniportDeviceContext,
    _In_ ULONG DeviceUid,
    _In_ DEVICE_POWER_STATE DevicePowerState,
    _In_ POWER_ACTION ActionType)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    UINT32 i;
    UNREFERENCED_PARAMETER(ActionType);

    if (dev == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    TraceEvents(
        TRACE_LEVEL_INFORMATION,
        TRACE_DEVICE,
        "power uid=%u state=%u",
        DeviceUid,
        (ULONG)DevicePowerState);

    if (DeviceUid != DISPLAY_ADAPTER_HW_ID) {
        return STATUS_SUCCESS;
    }
    if (DevicePowerState == PowerDeviceD3 || DevicePowerState == PowerDeviceD2) {
        for (i = 0; i < dev->NumScanouts && i < VIRTIO_GPU_MAX_SCANOUTS; i++) {
            if (dev->Scanouts[i].ResourceId != 0) {
                (VOID)TvgpuDisableScanout(dev, i);
            }
        }
    }
    if (DevicePowerState == PowerDeviceD0) {
        (VOID)TvgpuRefreshDisplays(dev);
        TvgpuAppendLog("D0");
    }
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuNotifyAcpiEvent(
    _In_ PVOID MiniportDeviceContext,
    _In_ DXGK_EVENT_TYPE EventType,
    _In_ ULONG Event,
    _In_ PVOID Argument,
    _Out_ PULONG AcpiFlags)
{
    UNREFERENCED_PARAMETER(MiniportDeviceContext);
    UNREFERENCED_PARAMETER(EventType);
    UNREFERENCED_PARAMETER(Event);
    UNREFERENCED_PARAMETER(Argument);
    if (AcpiFlags != NULL) {
        *AcpiFlags = 0;
    }
    return STATUS_SUCCESS;
}

VOID
TvgpuResetDevice(_In_ PVOID MiniportDeviceContext)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    if (dev != NULL && dev->Common != NULL) {
        WRITE_REGISTER_UCHAR((volatile UINT8 *)(dev->Common + VIRTIO_PCI_COMMON_STATUS), 0);
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_DEVICE, "adapter reset");
    }
}
