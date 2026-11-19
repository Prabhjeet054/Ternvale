/* Display-only WDDM miniport (KMDOD) for Ternvale virtio-gpu.
 *
 * Guest-hostile: every length and index from the host is range-checked
 * before use (drivers/ Windows driver rules).
 */
#pragma once

#include <ntddk.h>
#include <dispmprt.h>
#include <ntstrsafe.h>

#include "virtio_gpu_abi.h"
#include "virtio_pci.h"

#define TVGPU_POOL_TAG 'ugvT'
#define TVGPU_MAX_BACKING 64u
#define TVGPU_EDID_MIN 128u
#define TVGPU_CURSOR_MAX 64u

typedef struct _TVGPU_VRING {
    PVOID DescVa;
    PVOID AvailVa;
    PVOID UsedVa;
    PHYSICAL_ADDRESS DescPa;
    PHYSICAL_ADDRESS AvailPa;
    PHYSICAL_ADDRESS UsedPa;
    UINT16 Size;
    UINT16 NextAvail;
    UINT16 LastUsed;
    UINT16 NotifyOff;
    BOOLEAN Ready;
} TVGPU_VRING;

typedef struct _TVGPU_SCANOUT {
    BOOLEAN Enabled;
    BOOLEAN Connected;
    BOOLEAN HasEdid;
    UINT32 Width;
    UINT32 Height;
    UINT32 ResourceId;
    UINT32 Pitch;
    PHYSICAL_ADDRESS PrimaryPa;
    UINT32 EdidSize;
    UINT8 Edid[TVGPU_EDID_MIN];
} TVGPU_SCANOUT;

typedef struct _TVGPU_DEVICE {
    DEVICE_OBJECT *PhysicalDeviceObject;
    DXGKRNL_INTERFACE Dxgk;
    PVOID DxgkContext;

    PVOID Bar0;
    SIZE_T Bar0Size;
    volatile UINT8 *Common;
    volatile UINT8 *Isr;
    volatile UINT8 *Notify;
    volatile VIRTIO_GPU_CONFIG *GpuCfg;
    UINT32 NotifyMultiplier;
    UINT64 DriverFeatures;

    TVGPU_VRING Controlq;
    TVGPU_VRING Cursorq;
    KEVENT ControlDone;
    KEVENT CursorDone;
    KMUTEX CmdMutex;
    KSPIN_LOCK Lock;
    volatile LONG ConfigPending;
    PIO_WORKITEM ConfigWork;

    UINT32 NumScanouts;
    BOOLEAN EdidOffered;
    BOOLEAN CursorqReady;
    /* Host ACKs cursorq but does not composite. Default: software cursor. */
    BOOLEAN HardwareCursor;
    UINT32 NextResourceId;
    UINT32 CursorResourceId;
    TVGPU_SCANOUT Scanouts[VIRTIO_GPU_MAX_SCANOUTS];

    UINT32 Fence;
    PVOID CmdBuf;
    PHYSICAL_ADDRESS CmdBufPa;
    SIZE_T CmdBufSize;
    PVOID RspBuf;
    PHYSICAL_ADDRESS RspBufPa;
    SIZE_T RspBufSize;
} TVGPU_DEVICE;

NTSTATUS TvgpuAppendLog(_In_ PCSTR Message);

NTSTATUS TvgpuValidateHostU32(_In_ UINT32 Value, _In_ UINT32 MaxInclusive, _In_ PCSTR What);
NTSTATUS TvgpuValidateDimension(_In_ UINT32 Width, _In_ UINT32 Height);
NTSTATUS TvgpuValidateScanoutId(_In_ const TVGPU_DEVICE *Dev, _In_ UINT32 Id);
BOOLEAN TvgpuEdidLooksValid(_In_reads_(Length) const UINT8 *Edid, _In_ UINT32 Length);

NTSTATUS TvgpuMapBars(_Inout_ TVGPU_DEVICE *Dev, _In_ PCM_RESOURCE_LIST Translated);
NTSTATUS TvgpuVirtioStart(_Inout_ TVGPU_DEVICE *Dev);
VOID TvgpuVirtioStop(_Inout_ TVGPU_DEVICE *Dev);
NTSTATUS TvgpuSyncCommand(
    _Inout_ TVGPU_DEVICE *Dev,
    _In_ BOOLEAN CursorQueue,
    _In_reads_bytes_(ReqLen) PVOID Request,
    _In_ UINT32 ReqLen,
    _Out_writes_bytes_(RspLen) PVOID Response,
    _In_ UINT32 RspLen,
    _In_ UINT32 ExpectedOk);

NTSTATUS TvgpuRefreshDisplays(_Inout_ TVGPU_DEVICE *Dev);
NTSTATUS TvgpuPresentScanout(
    _Inout_ TVGPU_DEVICE *Dev,
    _In_ UINT32 Scanout,
    _In_ PHYSICAL_ADDRESS Primary,
    _In_ UINT32 Width,
    _In_ UINT32 Height,
    _In_ UINT32 Pitch,
    _In_ UINT32 Format);
NTSTATUS TvgpuDisableScanout(_Inout_ TVGPU_DEVICE *Dev, _In_ UINT32 Scanout);

NTSTATUS TvgpuCursorUpdate(
    _Inout_ TVGPU_DEVICE *Dev,
    _In_ INT32 X,
    _In_ INT32 Y,
    _In_ UINT32 ResourceId,
    _In_ UINT32 HotX,
    _In_ UINT32 HotY,
    _In_ BOOLEAN Visible);
NTSTATUS TvgpuCursorMove(_Inout_ TVGPU_DEVICE *Dev, _In_ INT32 X, _In_ INT32 Y);

BOOLEAN TvgpuInterruptRoutine(_In_ PVOID MiniportDeviceContext, _In_ ULONG MessageNumber);
VOID TvgpuDpcRoutine(_In_ PVOID MiniportDeviceContext);
VOID TvgpuConfigWork(_In_ DEVICE_OBJECT *DeviceObject, _In_opt_ PVOID Context);

NTSTATUS TvgpuAddDevice(_In_ DEVICE_OBJECT *PhysicalDeviceObject, _Outptr_ PVOID *MiniportDeviceContext);
NTSTATUS TvgpuStartDevice(
    _In_ PVOID MiniportDeviceContext,
    _In_ PDXGK_START_INFO DxgkStartInfo,
    _In_ PDXGKRNL_INTERFACE DxgkInterface,
    _Out_ PULONG NumberOfVideoPresentSources,
    _Out_ PULONG NumberOfChildren);
NTSTATUS TvgpuStopDevice(_In_ PVOID MiniportDeviceContext);
NTSTATUS TvgpuRemoveDevice(_In_ PVOID MiniportDeviceContext);
NTSTATUS TvgpuDispatchIoRequest(
    _In_ PVOID MiniportDeviceContext,
    _In_ ULONG VidPnSourceId,
    _In_ PVIDEO_REQUEST_PACKET VideoRequestPacket);
NTSTATUS TvgpuQueryChildRelations(
    _In_ PVOID MiniportDeviceContext,
    _Out_writes_bytes_(ChildRelationsSize) PDXGK_CHILD_DESCRIPTOR ChildRelations,
    _In_ ULONG ChildRelationsSize);
NTSTATUS TvgpuQueryChildStatus(
    _In_ PVOID MiniportDeviceContext,
    _Inout_ PDXGK_CHILD_STATUS ChildStatus,
    _In_ BOOLEAN NonDestructiveOnly);
NTSTATUS TvgpuQueryDeviceDescriptor(
    _In_ PVOID MiniportDeviceContext,
    _In_ ULONG ChildUid,
    _Inout_ PDXGK_DEVICE_DESCRIPTOR DeviceDescriptor);
NTSTATUS TvgpuSetPowerState(
    _In_ PVOID MiniportDeviceContext,
    _In_ ULONG DeviceUid,
    _In_ DEVICE_POWER_STATE DevicePowerState,
    _In_ POWER_ACTION ActionType);
NTSTATUS TvgpuNotifyAcpiEvent(
    _In_ PVOID MiniportDeviceContext,
    _In_ DXGK_EVENT_TYPE EventType,
    _In_ ULONG Event,
    _In_ PVOID Argument,
    _Out_ PULONG AcpiFlags);
VOID TvgpuResetDevice(_In_ PVOID MiniportDeviceContext);
VOID TvgpuUnload(VOID);

NTSTATUS TvgpuQueryAdapterInfo(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_QUERYADAPTERINFO *Query);
NTSTATUS TvgpuIsSupportedVidPn(_In_ PVOID MiniportDeviceContext, _Inout_ DXGKARG_ISSUPPORTEDVIDPN *Args);
NTSTATUS TvgpuRecommendFunctionalVidPn(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_RECOMMENDFUNCTIONALVIDPN *CONST Args);
NTSTATUS TvgpuEnumVidPnCofuncModality(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_ENUMVIDPNCOFUNCMODALITY *CONST Args);
NTSTATUS TvgpuSetVidPnSourceAddress(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_SETVIDPNSOURCEADDRESS *Args);
NTSTATUS TvgpuSetVidPnSourceVisibility(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_SETVIDPNSOURCEVISIBILITY *Args);
NTSTATUS TvgpuCommitVidPn(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_COMMITVIDPN *CONST Args);
NTSTATUS TvgpuUpdateActiveVidPnPresentPath(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_UPDATEACTIVEVIDPNPRESENTPATH *CONST Args);
NTSTATUS TvgpuRecommendMonitorModes(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_RECOMMENDMONITORMODES *CONST Args);
NTSTATUS TvgpuGetScanLine(_In_ PVOID MiniportDeviceContext, _Inout_ DXGKARG_GETSCANLINE *Args);
NTSTATUS TvgpuQueryVidPnHWCapability(
    _In_ PVOID MiniportDeviceContext,
    _Inout_ DXGKARG_QUERYVIDPNHWCAPABILITY *Args);
NTSTATUS TvgpuSetPointerPosition(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_SETPOINTERPOSITION *Args);
NTSTATUS TvgpuSetPointerShape(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_SETPOINTERSHAPE *Args);
NTSTATUS TvgpuStopDeviceAndReleasePostDisplayOwnership(
    _In_ PVOID MiniportDeviceContext,
    _In_ D3DDDI_VIDEO_PRESENT_TARGET_ID TargetId,
    _Out_ PDXGK_DISPLAY_INFORMATION DisplayInfo);
