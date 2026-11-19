#include "include/tvgpu.h"
#include "include/trace.h"
#include "cursor.tmh"

NTSTATUS
TvgpuCursorUpdate(
    _Inout_ TVGPU_DEVICE *Dev,
    _In_ INT32 X,
    _In_ INT32 Y,
    _In_ UINT32 ResourceId,
    _In_ UINT32 HotX,
    _In_ UINT32 HotY,
    _In_ BOOLEAN Visible)
{
    VIRTIO_GPU_UPDATE_CURSOR cmd;
    VIRTIO_GPU_CTRL_HDR rsp;

    if (!Dev->CursorqReady || !Dev->HardwareCursor) {
        return STATUS_NOT_SUPPORTED;
    }
    if (HotX > TVGPU_CURSOR_MAX || HotY > TVGPU_CURSOR_MAX) {
        TraceEvents(TRACE_LEVEL_WARNING, TRACE_DEVICE, "cursor hotspot rejected");
        return STATUS_INVALID_PARAMETER;
    }
    RtlZeroMemory(&cmd, sizeof(cmd));
    cmd.Hdr.Type = VIRTIO_GPU_CMD_UPDATE_CURSOR;
    cmd.Hdr.Flags = VIRTIO_GPU_FLAG_FENCE;
    cmd.Hdr.FenceId = ++Dev->Fence;
    cmd.Pos.X = (UINT32)X;
    cmd.Pos.Y = (UINT32)Y;
    cmd.ResourceId = Visible ? ResourceId : 0;
    cmd.HotX = HotX;
    cmd.HotY = HotY;
    return TvgpuSyncCommand(
        Dev, TRUE, &cmd, sizeof(cmd), &rsp, sizeof(rsp), VIRTIO_GPU_RESP_OK_NODATA);
}

NTSTATUS
TvgpuCursorMove(_Inout_ TVGPU_DEVICE *Dev, _In_ INT32 X, _In_ INT32 Y)
{
    VIRTIO_GPU_UPDATE_CURSOR cmd;
    VIRTIO_GPU_CTRL_HDR rsp;

    if (!Dev->CursorqReady || !Dev->HardwareCursor) {
        return STATUS_NOT_SUPPORTED;
    }
    RtlZeroMemory(&cmd, sizeof(cmd));
    cmd.Hdr.Type = VIRTIO_GPU_CMD_MOVE_CURSOR;
    cmd.Hdr.Flags = VIRTIO_GPU_FLAG_FENCE;
    cmd.Hdr.FenceId = ++Dev->Fence;
    cmd.Pos.X = (UINT32)X;
    cmd.Pos.Y = (UINT32)Y;
    cmd.ResourceId = Dev->CursorResourceId;
    return TvgpuSyncCommand(
        Dev, TRUE, &cmd, sizeof(cmd), &rsp, sizeof(rsp), VIRTIO_GPU_RESP_OK_NODATA);
}

NTSTATUS
TvgpuSetPointerPosition(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_SETPOINTERPOSITION *Args)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;

    if (dev == NULL || Args == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!dev->HardwareCursor) {
        return STATUS_SUCCESS;
    }
    if (Args->Flags.Visible) {
        return TvgpuCursorMove(dev, Args->X, Args->Y);
    }
    return TvgpuCursorUpdate(dev, Args->X, Args->Y, 0, 0, 0, FALSE);
}

NTSTATUS
TvgpuSetPointerShape(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_SETPOINTERSHAPE *Args)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;

    if (dev == NULL || Args == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!dev->HardwareCursor) {
        /* Windows draws a software cursor. Host cursorq is ACK-only. */
        return STATUS_SUCCESS;
    }
    if (Args->Width > TVGPU_CURSOR_MAX || Args->Height > TVGPU_CURSOR_MAX) {
        return STATUS_INVALID_PARAMETER;
    }
    /* TODO(verify): create a 64x64 cursor resource from Args->pPixels when the
     * host composites cursorq (currently ACK with no draw). */
    return TvgpuCursorUpdate(
        dev,
        0,
        0,
        dev->CursorResourceId,
        Args->XHot,
        Args->YHot,
        TRUE);
}
