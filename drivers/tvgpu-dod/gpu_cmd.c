#include "include/tvgpu.h"
#include "include/trace.h"
#include "gpu_cmd.tmh"

static VOID
HdrInit(_Out_ VIRTIO_GPU_CTRL_HDR *Hdr, _In_ UINT32 Type, _In_ UINT32 Fence)
{
    RtlZeroMemory(Hdr, sizeof(*Hdr));
    Hdr->Type = Type;
    Hdr->Flags = VIRTIO_GPU_FLAG_FENCE;
    Hdr->FenceId = Fence;
}

NTSTATUS
TvgpuRefreshDisplays(_Inout_ TVGPU_DEVICE *Dev)
{
    VIRTIO_GPU_CTRL_HDR req;
    VIRTIO_GPU_RESP_DISPLAY_INFO info;
    VIRTIO_GPU_GET_EDID edidReq;
    VIRTIO_GPU_RESP_EDID edidRsp;
    NTSTATUS status;
    UINT32 i;
    UINT32 enabled = 0;

    RtlZeroMemory(&req, sizeof(req));
    HdrInit(&req, VIRTIO_GPU_CMD_GET_DISPLAY_INFO, ++Dev->Fence);
    RtlZeroMemory(&info, sizeof(info));
    status = TvgpuSyncCommand(
        Dev,
        FALSE,
        &req,
        sizeof(req),
        &info,
        sizeof(info),
        VIRTIO_GPU_RESP_OK_DISPLAY_INFO);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    for (i = 0; i < VIRTIO_GPU_MAX_SCANOUTS; i++) {
        VIRTIO_GPU_DISPLAY_ONE *mode = &info.Pmodes[i];
        BOOLEAN on = FALSE;
        if (i >= Dev->NumScanouts) {
            if (mode->Enabled != 0) {
                TraceEvents(
                    TRACE_LEVEL_WARNING,
                    TRACE_DEVICE,
                    "host enabled scanout %u beyond num_scanouts %u",
                    i,
                    Dev->NumScanouts);
            }
            Dev->Scanouts[i].Enabled = FALSE;
            Dev->Scanouts[i].Connected = FALSE;
            continue;
        }
        if (mode->Enabled != 0) {
            status = TvgpuValidateDimension(mode->Rect.Width, mode->Rect.Height);
            if (!NT_SUCCESS(status)) {
                Dev->Scanouts[i].Enabled = FALSE;
                Dev->Scanouts[i].Connected = FALSE;
                continue;
            }
            on = TRUE;
            Dev->Scanouts[i].Width = mode->Rect.Width;
            Dev->Scanouts[i].Height = mode->Rect.Height;
        }
        Dev->Scanouts[i].Enabled = on;
        Dev->Scanouts[i].Connected = on;
        if (on) {
            enabled++;
        }

        Dev->Scanouts[i].HasEdid = FALSE;
        if (!on || !Dev->EdidOffered) {
            continue;
        }
        RtlZeroMemory(&edidReq, sizeof(edidReq));
        HdrInit(&edidReq.Hdr, VIRTIO_GPU_CMD_GET_EDID, ++Dev->Fence);
        edidReq.Scanout = i;
        RtlZeroMemory(&edidRsp, sizeof(edidRsp));
        status = TvgpuSyncCommand(
            Dev,
            FALSE,
            &edidReq,
            sizeof(edidReq),
            &edidRsp,
            sizeof(edidRsp),
            VIRTIO_GPU_RESP_OK_EDID);
        if (!NT_SUCCESS(status)) {
            continue;
        }
        status = TvgpuValidateHostU32(edidRsp.Size, VIRTIO_GPU_EDID_WIRE_LEN, "edid.size");
        if (!NT_SUCCESS(status) || edidRsp.Size < TVGPU_EDID_MIN) {
            TraceEvents(TRACE_LEVEL_WARNING, TRACE_DEVICE, "GET_EDID size %u rejected", edidRsp.Size);
            continue;
        }
        if (!TvgpuEdidLooksValid(edidRsp.Edid, edidRsp.Size)) {
            continue;
        }
        RtlCopyMemory(Dev->Scanouts[i].Edid, edidRsp.Edid, TVGPU_EDID_MIN);
        Dev->Scanouts[i].EdidSize = TVGPU_EDID_MIN;
        Dev->Scanouts[i].HasEdid = TRUE;
    }

    TraceEvents(
        TRACE_LEVEL_INFORMATION,
        TRACE_DEVICE,
        "display info enabled=%u num_scanouts=%u",
        enabled,
        Dev->NumScanouts);
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuDisableScanout(_Inout_ TVGPU_DEVICE *Dev, _In_ UINT32 Scanout)
{
    VIRTIO_GPU_SET_SCANOUT cmd;
    VIRTIO_GPU_CTRL_HDR rsp;
    VIRTIO_GPU_RESOURCE_UNREF unref;
    NTSTATUS status;
    UINT32 rid;

    status = TvgpuValidateScanoutId(Dev, Scanout);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    RtlZeroMemory(&cmd, sizeof(cmd));
    HdrInit(&cmd.Hdr, VIRTIO_GPU_CMD_SET_SCANOUT, ++Dev->Fence);
    cmd.ScanoutId = Scanout;
    cmd.ResourceId = 0;
    status = TvgpuSyncCommand(
        Dev, FALSE, &cmd, sizeof(cmd), &rsp, sizeof(rsp), VIRTIO_GPU_RESP_OK_NODATA);
    rid = Dev->Scanouts[Scanout].ResourceId;
    Dev->Scanouts[Scanout].ResourceId = 0;
    if (rid != 0) {
        RtlZeroMemory(&unref, sizeof(unref));
        HdrInit(&unref.Hdr, VIRTIO_GPU_CMD_RESOURCE_UNREF, ++Dev->Fence);
        unref.ResourceId = rid;
        (VOID)TvgpuSyncCommand(
            Dev, FALSE, &unref, sizeof(unref), &rsp, sizeof(rsp), VIRTIO_GPU_RESP_OK_NODATA);
    }
    return status;
}

NTSTATUS
TvgpuPresentScanout(
    _Inout_ TVGPU_DEVICE *Dev,
    _In_ UINT32 Scanout,
    _In_ PHYSICAL_ADDRESS Primary,
    _In_ UINT32 Width,
    _In_ UINT32 Height,
    _In_ UINT32 Pitch,
    _In_ UINT32 Format)
{
    NTSTATUS status;
    VIRTIO_GPU_CTRL_HDR rsp;
    VIRTIO_GPU_RESOURCE_CREATE_2D create;
    VIRTIO_GPU_RESOURCE_UNREF unref;
    UINT8 attach[sizeof(VIRTIO_GPU_RESOURCE_ATTACH_BACKING) + sizeof(VIRTIO_GPU_MEM_ENTRY)];
    VIRTIO_GPU_RESOURCE_ATTACH_BACKING *ab;
    VIRTIO_GPU_MEM_ENTRY *ent;
    VIRTIO_GPU_TRANSFER_TO_HOST_2D xfer;
    VIRTIO_GPU_SET_SCANOUT set;
    VIRTIO_GPU_RESOURCE_FLUSH flush;
    UINT32 rid;
    UINT32 bytes;

    status = TvgpuValidateScanoutId(Dev, Scanout);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = TvgpuValidateDimension(Width, Height);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (Pitch < Width * 4 || Pitch > Width * 16) {
        TraceEvents(TRACE_LEVEL_WARNING, TRACE_PRESENT, "pitch %u rejected", Pitch);
        return STATUS_INVALID_PARAMETER;
    }
    if (Primary.QuadPart == 0) {
        return STATUS_INVALID_PARAMETER;
    }
    bytes = Pitch * Height;
    if (bytes / Pitch != Height) {
        return STATUS_INTEGER_OVERFLOW;
    }

    rid = Dev->Scanouts[Scanout].ResourceId;
    if (rid != 0 &&
        (Dev->Scanouts[Scanout].Width != Width || Dev->Scanouts[Scanout].Height != Height)) {
        RtlZeroMemory(&unref, sizeof(unref));
        HdrInit(&unref.Hdr, VIRTIO_GPU_CMD_RESOURCE_UNREF, ++Dev->Fence);
        unref.ResourceId = rid;
        (VOID)TvgpuSyncCommand(
            Dev, FALSE, &unref, sizeof(unref), &rsp, sizeof(rsp), VIRTIO_GPU_RESP_OK_NODATA);
        rid = 0;
    }
    if (rid == 0) {
        rid = ++Dev->NextResourceId;
        if (rid == 0) {
            rid = ++Dev->NextResourceId;
        }
        RtlZeroMemory(&create, sizeof(create));
        HdrInit(&create.Hdr, VIRTIO_GPU_CMD_RESOURCE_CREATE_2D, ++Dev->Fence);
        create.ResourceId = rid;
        create.Format = Format;
        create.Width = Width;
        create.Height = Height;
        status = TvgpuSyncCommand(
            Dev, FALSE, &create, sizeof(create), &rsp, sizeof(rsp), VIRTIO_GPU_RESP_OK_NODATA);
        if (!NT_SUCCESS(status)) {
            return status;
        }
        RtlZeroMemory(attach, sizeof(attach));
        ab = (VIRTIO_GPU_RESOURCE_ATTACH_BACKING *)attach;
        HdrInit(&ab->Hdr, VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING, ++Dev->Fence);
        ab->ResourceId = rid;
        ab->NrEntries = 1;
        ent = (VIRTIO_GPU_MEM_ENTRY *)(ab + 1);
        ent->Addr = (UINT64)Primary.QuadPart;
        ent->Length = bytes;
        status = TvgpuSyncCommand(
            Dev, FALSE, attach, sizeof(attach), &rsp, sizeof(rsp), VIRTIO_GPU_RESP_OK_NODATA);
        if (!NT_SUCCESS(status)) {
            return status;
        }
        Dev->Scanouts[Scanout].ResourceId = rid;
    }

    RtlZeroMemory(&xfer, sizeof(xfer));
    HdrInit(&xfer.Hdr, VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D, ++Dev->Fence);
    xfer.Rect.Width = Width;
    xfer.Rect.Height = Height;
    xfer.Offset = 0;
    xfer.ResourceId = rid;
    status = TvgpuSyncCommand(
        Dev, FALSE, &xfer, sizeof(xfer), &rsp, sizeof(rsp), VIRTIO_GPU_RESP_OK_NODATA);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    RtlZeroMemory(&set, sizeof(set));
    HdrInit(&set.Hdr, VIRTIO_GPU_CMD_SET_SCANOUT, ++Dev->Fence);
    set.Rect.Width = Width;
    set.Rect.Height = Height;
    set.ScanoutId = Scanout;
    set.ResourceId = rid;
    status = TvgpuSyncCommand(
        Dev, FALSE, &set, sizeof(set), &rsp, sizeof(rsp), VIRTIO_GPU_RESP_OK_NODATA);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    RtlZeroMemory(&flush, sizeof(flush));
    HdrInit(&flush.Hdr, VIRTIO_GPU_CMD_RESOURCE_FLUSH, ++Dev->Fence);
    flush.Rect.Width = Width;
    flush.Rect.Height = Height;
    flush.ResourceId = rid;
    status = TvgpuSyncCommand(
        Dev, FALSE, &flush, sizeof(flush), &rsp, sizeof(rsp), VIRTIO_GPU_RESP_OK_NODATA);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    Dev->Scanouts[Scanout].Width = Width;
    Dev->Scanouts[Scanout].Height = Height;
    Dev->Scanouts[Scanout].Pitch = Pitch;
    Dev->Scanouts[Scanout].PrimaryPa = Primary;
    Dev->Scanouts[Scanout].Enabled = TRUE;
    TraceEvents(
        TRACE_LEVEL_INFORMATION,
        TRACE_PRESENT,
        "present scanout %u %ux%u rid %u",
        Scanout,
        Width,
        Height,
        rid);
    return STATUS_SUCCESS;
}
