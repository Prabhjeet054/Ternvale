/* virtio-gpu 3D: CTX_CREATE + SUBMIT_3D for the tvvk ICD (virtio 1.2 §5.7). */

#include "include/tvgpu.h"
#include "include/trace.h"
#include "gpu3d.tmh"

NTSTATUS
TvgpuSubmitUserCs(_Inout_ TVGPU_DEVICE *Dev, _In_reads_bytes_(Len) PVOID Cs, _In_ UINT32 Len)
{
    VIRTIO_GPU_CTX_CREATE create;
    VIRTIO_GPU_CTRL_HDR rsp;
    VIRTIO_GPU_SUBMIT_3D *submit;
    UINT8 *req;
    UINT32 reqLen;
    NTSTATUS status;

    if (Dev == NULL || Cs == NULL || Len == 0 || Len > 65536u - 32u) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!Dev->ThreeD) {
        TraceEvents(TRACE_LEVEL_WARNING, TRACE_DEVICE, "SUBMIT_3D rejected: host gpu3d off");
        return STATUS_DEVICE_NOT_READY;
    }

    if (!Dev->CtxReady) {
        RtlZeroMemory(&create, sizeof(create));
        create.Hdr.Type = VIRTIO_GPU_CMD_CTX_CREATE;
        create.Hdr.Flags = VIRTIO_GPU_FLAG_FENCE;
        create.Hdr.FenceId = ++Dev->Fence;
        create.Hdr.CtxId = 1;
        create.Nlen = 4;
        create.ContextInit = VIRTIO_GPU_CAPSET_VENUS;
        create.DebugName[0] = 't';
        create.DebugName[1] = 'v';
        create.DebugName[2] = 'v';
        create.DebugName[3] = 'k';
        RtlZeroMemory(&rsp, sizeof(rsp));
        status = TvgpuSyncCommand(
            Dev,
            FALSE,
            &create,
            sizeof(create),
            &rsp,
            sizeof(rsp),
            VIRTIO_GPU_RESP_OK_NODATA);
        if (!NT_SUCCESS(status)) {
            TvgpuAppendLog("CTX_CREATE failed");
            return status;
        }
        Dev->CtxId = 1;
        Dev->CtxReady = TRUE;
        TvgpuAppendLog("CTX_CREATE ok");
    }

    reqLen = sizeof(VIRTIO_GPU_SUBMIT_3D) + Len;
    req = (UINT8 *)ExAllocatePool2(POOL_FLAG_NON_PAGED, reqLen, TVGPU_POOL_TAG);
    if (req == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(req, reqLen);
    submit = (VIRTIO_GPU_SUBMIT_3D *)req;
    submit->Hdr.Type = VIRTIO_GPU_CMD_SUBMIT_3D;
    submit->Hdr.Flags = VIRTIO_GPU_FLAG_FENCE;
    submit->Hdr.FenceId = ++Dev->Fence;
    submit->Hdr.CtxId = Dev->CtxId;
    submit->Size = Len;
    RtlCopyMemory(req + sizeof(VIRTIO_GPU_SUBMIT_3D), Cs, Len);
    RtlZeroMemory(&rsp, sizeof(rsp));
    status = TvgpuSyncCommand(
        Dev,
        FALSE,
        req,
        reqLen,
        &rsp,
        sizeof(rsp),
        VIRTIO_GPU_RESP_OK_NODATA);
    ExFreePoolWithTag(req, TVGPU_POOL_TAG);
    if (!NT_SUCCESS(status)) {
        TvgpuAppendLog("SUBMIT_3D failed");
    }
    return status;
}
