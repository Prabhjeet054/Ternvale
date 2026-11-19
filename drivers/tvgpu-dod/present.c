#include "include/tvgpu.h"
#include "include/trace.h"
#include "present.tmh"

NTSTATUS
TvgpuSetVidPnSourceAddress(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_SETVIDPNSOURCEADDRESS *Args)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    UINT32 src;
    UINT32 width;
    UINT32 height;
    UINT32 pitch;
    PHYSICAL_ADDRESS pa;

    if (dev == NULL || Args == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    src = Args->VidPnSourceId;
    if (!NT_SUCCESS(TvgpuValidateScanoutId(dev, src))) {
        return STATUS_GRAPHICS_INVALID_VIDEO_PRESENT_SOURCE;
    }
    width = dev->Scanouts[src].Width;
    height = dev->Scanouts[src].Height;
    if (width == 0 || height == 0) {
        return STATUS_GRAPHICS_INVALID_VIDEO_PRESENT_SOURCE_MODE;
    }
    pitch = width * 4;
    pa.QuadPart = (LONGLONG)Args->PrimaryAddress.QuadPart;
    if (Args->PrimarySegment != 0) {
        /* Segment-relative: KMDOD display-only uses physical PrimaryAddress. */
        TraceEvents(
            TRACE_LEVEL_VERBOSE,
            TRACE_PRESENT,
            "SetVidPnSourceAddress segment %u",
            Args->PrimarySegment);
    }
    TraceEvents(
        TRACE_LEVEL_INFORMATION,
        TRACE_PRESENT,
        "SetVidPnSourceAddress src=%u pa=%I64x",
        src,
        pa.QuadPart);
    return TvgpuPresentScanout(
        dev,
        src,
        pa,
        width,
        height,
        pitch,
        VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM);
}

NTSTATUS
TvgpuSetVidPnSourceVisibility(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_SETVIDPNSOURCEVISIBILITY *Args)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;

    if (dev == NULL || Args == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!NT_SUCCESS(TvgpuValidateScanoutId(dev, Args->VidPnSourceId))) {
        return STATUS_GRAPHICS_INVALID_VIDEO_PRESENT_SOURCE;
    }
    TraceEvents(
        TRACE_LEVEL_INFORMATION,
        TRACE_PRESENT,
        "visibility src=%u visible=%u",
        Args->VidPnSourceId,
        Args->Visible);
    if (!Args->Visible) {
        return TvgpuDisableScanout(dev, Args->VidPnSourceId);
    }
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuGetScanLine(_In_ PVOID MiniportDeviceContext, _Inout_ DXGKARG_GETSCANLINE *Args)
{
    UNREFERENCED_PARAMETER(MiniportDeviceContext);
    if (Args == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    Args->InVBlank = TRUE;
    Args->ScanLine = 0;
    return STATUS_SUCCESS;
}
