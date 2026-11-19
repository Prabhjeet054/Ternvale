#include "include/tvgpu.h"
#include "include/trace.h"
#include "virtio.tmh"

#define DESC_SIZE (16u * TVGPU_QUEUE_SIZE)
#define AVAIL_SIZE (6u + 2u * TVGPU_QUEUE_SIZE)
#define USED_SIZE (6u + 8u * TVGPU_QUEUE_SIZE)

static UINT8
CommonRead8(_In_ TVGPU_DEVICE *Dev, _In_ UINT32 Off)
{
    return READ_REGISTER_UCHAR((volatile UINT8 *)(Dev->Common + Off));
}

static UINT16
CommonRead16(_In_ TVGPU_DEVICE *Dev, _In_ UINT32 Off)
{
    return READ_REGISTER_USHORT((volatile USHORT *)(Dev->Common + Off));
}

static UINT32
CommonRead32(_In_ TVGPU_DEVICE *Dev, _In_ UINT32 Off)
{
    return READ_REGISTER_ULONG((volatile ULONG *)(Dev->Common + Off));
}

static VOID
CommonWrite8(_In_ TVGPU_DEVICE *Dev, _In_ UINT32 Off, _In_ UINT8 Value)
{
    WRITE_REGISTER_UCHAR((volatile UINT8 *)(Dev->Common + Off), Value);
}

static VOID
CommonWrite16(_In_ TVGPU_DEVICE *Dev, _In_ UINT32 Off, _In_ UINT16 Value)
{
    WRITE_REGISTER_USHORT((volatile USHORT *)(Dev->Common + Off), Value);
}

static VOID
CommonWrite32(_In_ TVGPU_DEVICE *Dev, _In_ UINT32 Off, _In_ UINT32 Value)
{
    WRITE_REGISTER_ULONG((volatile ULONG *)(Dev->Common + Off), Value);
}

static VOID
CommonWrite64(_In_ TVGPU_DEVICE *Dev, _In_ UINT32 Off, _In_ UINT64 Value)
{
    CommonWrite32(Dev, Off, (UINT32)Value);
    CommonWrite32(Dev, Off + 4, (UINT32)(Value >> 32));
}

NTSTATUS
TvgpuMapBars(_Inout_ TVGPU_DEVICE *Dev, _In_ PCM_RESOURCE_LIST Translated)
{
    ULONG i;
    ULONG k;
    CM_PARTIAL_RESOURCE_DESCRIPTOR *desc;

    if (Translated == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    for (i = 0; i < Translated->Count; i++) {
        for (k = 0; k < Translated->List[i].PartialResourceList.Count; k++) {
            desc = &Translated->List[i].PartialResourceList.PartialDescriptors[k];
            if (desc->Type != CmResourceTypeMemory) {
                continue;
            }
            Dev->Bar0Size = desc->u.Memory.Length;
            if (Dev->Bar0Size < (VIRTIO_PCI_NOTIFY_OFFSET + VIRTIO_PCI_REGION_SIZE)) {
                TraceEvents(TRACE_LEVEL_ERROR, TRACE_VIRTIO, "BAR0 too small %Iu", Dev->Bar0Size);
                return STATUS_DEVICE_PROTOCOL_ERROR;
            }
            Dev->Bar0 = MmMapIoSpaceEx(
                desc->u.Memory.Start,
                Dev->Bar0Size,
                PAGE_READWRITE | PAGE_NOCACHE);
            if (Dev->Bar0 == NULL) {
                return STATUS_INSUFFICIENT_RESOURCES;
            }
            Dev->Common = (volatile UINT8 *)Dev->Bar0 + VIRTIO_PCI_COMMON_OFFSET;
            Dev->Isr = (volatile UINT8 *)Dev->Bar0 + VIRTIO_PCI_ISR_OFFSET;
            Dev->Notify = (volatile UINT8 *)Dev->Bar0 + VIRTIO_PCI_NOTIFY_OFFSET;
            Dev->GpuCfg = (volatile VIRTIO_GPU_CONFIG *)((UINT8 *)Dev->Bar0 + VIRTIO_PCI_DEVICE_OFFSET);
            Dev->NotifyMultiplier = VIRTIO_PCI_NOTIFY_MULTIPLIER;
            TraceEvents(
                TRACE_LEVEL_INFORMATION,
                TRACE_VIRTIO,
                "mapped BAR0 size %Iu",
                Dev->Bar0Size);
            return STATUS_SUCCESS;
        }
    }
    return STATUS_DEVICE_CONFIGURATION_ERROR;
}

static NTSTATUS
AllocContig(_Out_ PVOID *Va, _Out_ PHYSICAL_ADDRESS *Pa, _In_ SIZE_T Bytes)
{
    PHYSICAL_ADDRESS low;
    PHYSICAL_ADDRESS high;
    PHYSICAL_ADDRESS skip;

    low.QuadPart = 0;
    high.QuadPart = 0xFFFFFFFFFFFFULL;
    skip.QuadPart = 0;
    *Va = MmAllocateContiguousNodeMemory(
        Bytes,
        low,
        high,
        skip,
        PAGE_READWRITE,
        MM_ANY_NODE_OK);
    if (*Va == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(*Va, Bytes);
    *Pa = MmGetPhysicalAddress(*Va);
    return STATUS_SUCCESS;
}

static NTSTATUS
SetupQueue(_Inout_ TVGPU_DEVICE *Dev, _In_ UINT16 Index, _Inout_ TVGPU_VRING *Ring)
{
    NTSTATUS status;
    UINT16 max;

    CommonWrite16(Dev, VIRTIO_PCI_COMMON_QSELECT, Index);
    max = CommonRead16(Dev, VIRTIO_PCI_COMMON_QSIZE);
    if (max == 0 || max < 4) {
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_VIRTIO, "queue %u size %u rejected", Index, max);
        return STATUS_DEVICE_PROTOCOL_ERROR;
    }
    Ring->Size = (UINT16)min(max, TVGPU_QUEUE_SIZE);
    status = AllocContig(&Ring->DescVa, &Ring->DescPa, DESC_SIZE);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = AllocContig(&Ring->AvailVa, &Ring->AvailPa, AVAIL_SIZE);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = AllocContig(&Ring->UsedVa, &Ring->UsedPa, USED_SIZE);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    Ring->NotifyOff = CommonRead16(Dev, VIRTIO_PCI_COMMON_QNOTIFY);
    CommonWrite16(Dev, VIRTIO_PCI_COMMON_QSIZE, Ring->Size);
    CommonWrite64(Dev, VIRTIO_PCI_COMMON_QDESC, (UINT64)Ring->DescPa.QuadPart);
    CommonWrite64(Dev, VIRTIO_PCI_COMMON_QDRIVER, (UINT64)Ring->AvailPa.QuadPart);
    CommonWrite64(Dev, VIRTIO_PCI_COMMON_QDEVICE, (UINT64)Ring->UsedPa.QuadPart);
    CommonWrite16(Dev, VIRTIO_PCI_COMMON_QENABLE, 1);
    Ring->Ready = TRUE;
    Ring->NextAvail = 0;
    Ring->LastUsed = 0;
    TraceEvents(
        TRACE_LEVEL_INFORMATION,
        TRACE_VIRTIO,
        "queue %u ready size %u notify_off %u",
        Index,
        Ring->Size,
        Ring->NotifyOff);
    return STATUS_SUCCESS;
}

static VOID
FreeRing(_Inout_ TVGPU_VRING *Ring)
{
    if (Ring->DescVa != NULL) {
        MmFreeContiguousMemory(Ring->DescVa);
    }
    if (Ring->AvailVa != NULL) {
        MmFreeContiguousMemory(Ring->AvailVa);
    }
    if (Ring->UsedVa != NULL) {
        MmFreeContiguousMemory(Ring->UsedVa);
    }
    RtlZeroMemory(Ring, sizeof(*Ring));
}

NTSTATUS
TvgpuVirtioStart(_Inout_ TVGPU_DEVICE *Dev)
{
    UINT32 featuresLo;
    UINT32 featuresHi;
    UINT8 status;
    NTSTATUS nt;
    UINT32 num;
    UINT32 i;

    CommonWrite8(Dev, VIRTIO_PCI_COMMON_STATUS, 0);
    CommonWrite8(Dev, VIRTIO_PCI_COMMON_STATUS, (UINT8)(VIRTIO_S_ACKNOWLEDGE | VIRTIO_S_DRIVER));

    CommonWrite32(Dev, VIRTIO_PCI_COMMON_DFSELECT, 0);
    featuresLo = CommonRead32(Dev, VIRTIO_PCI_COMMON_DFEATURE);
    CommonWrite32(Dev, VIRTIO_PCI_COMMON_DFSELECT, 1);
    featuresHi = CommonRead32(Dev, VIRTIO_PCI_COMMON_DFEATURE);
    Dev->DriverFeatures = ((UINT64)featuresHi << 32) | featuresLo;
    Dev->EdidOffered = (Dev->DriverFeatures & VIRTIO_GPU_F_EDID) != 0;
    Dev->DriverFeatures &= (VIRTIO_F_VERSION_1 | VIRTIO_GPU_F_EDID);

    CommonWrite32(Dev, VIRTIO_PCI_COMMON_GFSELECT, 0);
    CommonWrite32(Dev, VIRTIO_PCI_COMMON_GFEATURE, (UINT32)Dev->DriverFeatures);
    CommonWrite32(Dev, VIRTIO_PCI_COMMON_GFSELECT, 1);
    CommonWrite32(Dev, VIRTIO_PCI_COMMON_GFEATURE, (UINT32)(Dev->DriverFeatures >> 32));

    status = (UINT8)(VIRTIO_S_ACKNOWLEDGE | VIRTIO_S_DRIVER | VIRTIO_S_FEATURES_OK);
    CommonWrite8(Dev, VIRTIO_PCI_COMMON_STATUS, status);
    if ((CommonRead8(Dev, VIRTIO_PCI_COMMON_STATUS) & VIRTIO_S_FEATURES_OK) == 0) {
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_VIRTIO, "FEATURES_OK rejected");
        CommonWrite8(Dev, VIRTIO_PCI_COMMON_STATUS, VIRTIO_S_FAILED);
        return STATUS_DEVICE_PROTOCOL_ERROR;
    }

    num = READ_REGISTER_ULONG(&Dev->GpuCfg->NumScanouts);
    nt = TvgpuValidateHostU32(num, VIRTIO_GPU_MAX_SCANOUTS, "num_scanouts");
    if (!NT_SUCCESS(nt) || num == 0) {
        CommonWrite8(Dev, VIRTIO_PCI_COMMON_STATUS, VIRTIO_S_FAILED);
        return STATUS_DEVICE_PROTOCOL_ERROR;
    }
    Dev->NumScanouts = num;
    for (i = 0; i < VIRTIO_GPU_MAX_SCANOUTS; i++) {
        Dev->Scanouts[i].Enabled = FALSE;
        Dev->Scanouts[i].Connected = FALSE;
        Dev->Scanouts[i].ResourceId = 0;
    }

    nt = AllocContig(&Dev->CmdBuf, &Dev->CmdBufPa, PAGE_SIZE);
    if (!NT_SUCCESS(nt)) {
        return nt;
    }
    Dev->CmdBufSize = PAGE_SIZE;
    nt = AllocContig(&Dev->RspBuf, &Dev->RspBufPa, PAGE_SIZE);
    if (!NT_SUCCESS(nt)) {
        return nt;
    }
    Dev->RspBufSize = PAGE_SIZE;

    nt = SetupQueue(Dev, TVGPU_QUEUE_CONTROL, &Dev->Controlq);
    if (!NT_SUCCESS(nt)) {
        return nt;
    }
    nt = SetupQueue(Dev, TVGPU_QUEUE_CURSOR, &Dev->Cursorq);
    if (NT_SUCCESS(nt)) {
        Dev->CursorqReady = TRUE;
    } else {
        TraceEvents(TRACE_LEVEL_WARNING, TRACE_VIRTIO, "cursorq unavailable; software cursor");
        Dev->CursorqReady = FALSE;
        Dev->HardwareCursor = FALSE;
        nt = STATUS_SUCCESS;
    }

    CommonWrite8(
        Dev,
        VIRTIO_PCI_COMMON_STATUS,
        (UINT8)(VIRTIO_S_ACKNOWLEDGE | VIRTIO_S_DRIVER | VIRTIO_S_FEATURES_OK | VIRTIO_S_DRIVER_OK));
    TvgpuAppendLog("virtio-gpu DRIVER_OK");
    return STATUS_SUCCESS;
}

VOID
TvgpuVirtioStop(_Inout_ TVGPU_DEVICE *Dev)
{
    if (Dev->Common != NULL) {
        CommonWrite8(Dev, VIRTIO_PCI_COMMON_STATUS, 0);
    }
    FreeRing(&Dev->Controlq);
    FreeRing(&Dev->Cursorq);
    if (Dev->CmdBuf != NULL) {
        MmFreeContiguousMemory(Dev->CmdBuf);
        Dev->CmdBuf = NULL;
    }
    if (Dev->RspBuf != NULL) {
        MmFreeContiguousMemory(Dev->RspBuf);
        Dev->RspBuf = NULL;
    }
    if (Dev->Bar0 != NULL) {
        MmUnmapIoSpace(Dev->Bar0, Dev->Bar0Size);
        Dev->Bar0 = NULL;
    }
}

static VOID
Kick(_In_ TVGPU_DEVICE *Dev, _In_ const TVGPU_VRING *Ring)
{
    UINT32 off = (UINT32)Ring->NotifyOff * Dev->NotifyMultiplier;
    WRITE_REGISTER_USHORT((volatile USHORT *)(Dev->Notify + off), 0);
}

NTSTATUS
TvgpuSyncCommand(
    _Inout_ TVGPU_DEVICE *Dev,
    _In_ BOOLEAN CursorQueue,
    _In_reads_bytes_(ReqLen) PVOID Request,
    _In_ UINT32 ReqLen,
    _Out_writes_bytes_(RspLen) PVOID Response,
    _In_ UINT32 RspLen,
    _In_ UINT32 ExpectedOk)
{
    TVGPU_VRING *ring = CursorQueue ? &Dev->Cursorq : &Dev->Controlq;
    KEVENT *done = CursorQueue ? &Dev->CursorDone : &Dev->ControlDone;
    UINT16 head;
    UINT16 next;
    UINT16 *availIdx;
    UINT64 *desc;
    LARGE_INTEGER timeout;
    NTSTATUS status;
    UINT32 kind;
    VIRTIO_GPU_CTRL_HDR *hdr;

    if (!ring->Ready || Request == NULL || Response == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (ReqLen == 0 || ReqLen > Dev->CmdBufSize || RspLen == 0 || RspLen > Dev->RspBufSize) {
        return STATUS_BUFFER_TOO_SMALL;
    }
    if (KeGetCurrentIrql() > PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = KeWaitForSingleObject(&Dev->CmdMutex, Executive, KernelMode, FALSE, NULL);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    RtlCopyMemory(Dev->CmdBuf, Request, ReqLen);
    RtlZeroMemory(Dev->RspBuf, RspLen);
    KeClearEvent(done);

    head = (UINT16)(ring->NextAvail % ring->Size);
    next = (UINT16)((head + 1) % ring->Size);
    desc = (UINT64 *)ring->DescVa;
    /* struct virtio_desc: addr u64, len u32, flags u16, next u16 */
    desc[head * 2] = (UINT64)Dev->CmdBufPa.QuadPart;
    ((UINT32 *)(desc + head * 2 + 1))[0] = ReqLen;
    ((UINT16 *)((UINT8 *)desc + head * 16 + 12))[0] = (UINT16)VRING_DESC_F_NEXT;
    ((UINT16 *)((UINT8 *)desc + head * 16 + 14))[0] = next;
    desc[next * 2] = (UINT64)Dev->RspBufPa.QuadPart;
    ((UINT32 *)(desc + next * 2 + 1))[0] = RspLen;
    ((UINT16 *)((UINT8 *)desc + next * 16 + 12))[0] = (UINT16)VRING_DESC_F_WRITE;
    ((UINT16 *)((UINT8 *)desc + next * 16 + 14))[0] = 0;

    availIdx = (UINT16 *)((UINT8 *)ring->AvailVa + 2);
    ((UINT16 *)((UINT8 *)ring->AvailVa + 4))[ring->NextAvail % ring->Size] = head;
    MemoryBarrier();
    ring->NextAvail = (UINT16)(ring->NextAvail + 1);
    *availIdx = ring->NextAvail;
    MemoryBarrier();
    Kick(Dev, ring);

    {
        ULONG spins;
        UINT16 usedIdx;
        BOOLEAN complete = FALSE;
        for (spins = 0; spins < 400; spins++) {
            timeout.QuadPart = -10LL * 1000 * 50; /* 50 ms */
            (VOID)KeWaitForSingleObject(done, Executive, KernelMode, FALSE, &timeout);
            MemoryBarrier();
            usedIdx = *(volatile UINT16 *)((UINT8 *)ring->UsedVa + 2);
            if (usedIdx != ring->LastUsed) {
                ring->LastUsed = usedIdx;
                complete = TRUE;
                break;
            }
        }
        if (!complete) {
            TraceEvents(TRACE_LEVEL_ERROR, TRACE_VIRTIO, "virtio-gpu command timed out");
            KeReleaseMutex(&Dev->CmdMutex, FALSE);
            return STATUS_IO_TIMEOUT;
        }
        status = STATUS_SUCCESS;
    }

    RtlCopyMemory(Response, Dev->RspBuf, RspLen);
    hdr = (VIRTIO_GPU_CTRL_HDR *)Response;
    kind = hdr->Type;
    KeReleaseMutex(&Dev->CmdMutex, FALSE);

    if (kind != ExpectedOk) {
        TraceEvents(
            TRACE_LEVEL_WARNING,
            TRACE_VIRTIO,
            "virtio-gpu resp %#x expected %#x",
            kind,
            ExpectedOk);
        return STATUS_DEVICE_PROTOCOL_ERROR;
    }
    TraceEvents(TRACE_LEVEL_VERBOSE, TRACE_VIRTIO, "virtio-gpu resp %#x ok", kind);
    return STATUS_SUCCESS;
}
