#include "include/tvgpu.h"
#include "include/trace.h"
#include "validate.tmh"

NTSTATUS
TvgpuValidateHostU32(_In_ UINT32 Value, _In_ UINT32 MaxInclusive, _In_ PCSTR What)
{
    if (Value > MaxInclusive) {
        TraceEvents(
            TRACE_LEVEL_WARNING,
            TRACE_DEVICE,
            "host value %s=%u exceeds max %u",
            What,
            Value,
            MaxInclusive);
        TvgpuAppendLog("host value rejected");
        return STATUS_DEVICE_PROTOCOL_ERROR;
    }
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuValidateDimension(_In_ UINT32 Width, _In_ UINT32 Height)
{
    if (Width == 0 || Height == 0 || Width > TVGPU_MAX_DIMENSION || Height > TVGPU_MAX_DIMENSION) {
        TraceEvents(
            TRACE_LEVEL_WARNING,
            TRACE_DEVICE,
            "host dimension %ux%u rejected",
            Width,
            Height);
        return STATUS_DEVICE_PROTOCOL_ERROR;
    }
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuValidateScanoutId(_In_ const TVGPU_DEVICE *Dev, _In_ UINT32 Id)
{
    if (Dev == NULL || Id >= Dev->NumScanouts || Id >= VIRTIO_GPU_MAX_SCANOUTS) {
        TraceEvents(TRACE_LEVEL_WARNING, TRACE_DEVICE, "host scanout id %u rejected", Id);
        return STATUS_DEVICE_PROTOCOL_ERROR;
    }
    return STATUS_SUCCESS;
}

BOOLEAN
TvgpuEdidLooksValid(_In_reads_(Length) const UINT8 *Edid, _In_ UINT32 Length)
{
    static const UINT8 kHeader[8] = {0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00};
    UINT32 i;
    UINT8 sum = 0;

    if (Edid == NULL || Length < TVGPU_EDID_MIN || Length > VIRTIO_GPU_EDID_WIRE_LEN) {
        TraceEvents(TRACE_LEVEL_WARNING, TRACE_DEVICE, "EDID length %u rejected", Length);
        return FALSE;
    }
    if (RtlCompareMemory(Edid, kHeader, sizeof(kHeader)) != sizeof(kHeader)) {
        TraceEvents(TRACE_LEVEL_WARNING, TRACE_DEVICE, "EDID header rejected");
        return FALSE;
    }
    for (i = 0; i < TVGPU_EDID_MIN; i++) {
        sum = (UINT8)(sum + Edid[i]);
    }
    if (sum != 0) {
        TraceEvents(TRACE_LEVEL_WARNING, TRACE_DEVICE, "EDID checksum rejected");
        return FALSE;
    }
    return TRUE;
}
