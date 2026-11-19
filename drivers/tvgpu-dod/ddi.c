#include "include/tvgpu.h"
#include "include/trace.h"
#include "ddi.tmh"

NTSTATUS
TvgpuQueryAdapterInfo(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_QUERYADAPTERINFO *Query)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    DXGK_DRIVERCAPS *caps;

    if (dev == NULL || Query == NULL || Query->pOutputData == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    switch (Query->Type) {
    case DXGKQAITYPE_DRIVERCAPS:
        if (Query->OutputDataSize < sizeof(DXGK_DRIVERCAPS)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        caps = (DXGK_DRIVERCAPS *)Query->pOutputData;
        RtlZeroMemory(caps, sizeof(*caps));
        caps->HighestAcceptableAddress.QuadPart = (LONGLONG)-1;
        caps->MaxAllocationListSlotId = 16;
        caps->ApertureSegmentCount = 0;
        caps->MaxPointerWidth = 0;
        caps->MaxPointerHeight = 0;
        caps->PointerCaps.Value = 0;
        if (dev->HardwareCursor && dev->CursorqReady) {
            caps->MaxPointerWidth = TVGPU_CURSOR_MAX;
            caps->MaxPointerHeight = TVGPU_CURSOR_MAX;
            caps->PointerCaps.Color = 1;
        }
        caps->InterruptMessageNumber = 0;
        caps->NumberOfSwizzlingRanges = 0;
        caps->MaxOverlays = 0;
        caps->PresentationCaps.NoScreenToScreenBlt = 1;
        caps->PresentationCaps.NoOverlapRequired = 1;
        caps->PresentationCaps.SupportKernelModeCommandBuffer = 0;
        caps->FlipCaps.FlipOnVSyncMmIo = 0;
        caps->SchedulingCaps.PreemptionAware = 0;
        caps->MemoryManagementCaps.PagingNode = 0;
        caps->WDDMVersion = DXGKDDI_WDDMv1_3;
        caps->SupportNonVGA = TRUE;
        TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DRIVER, "QueryAdapterInfo DRIVERCAPS");
        return STATUS_SUCCESS;
    default:
        return STATUS_NOT_SUPPORTED;
    }
}
