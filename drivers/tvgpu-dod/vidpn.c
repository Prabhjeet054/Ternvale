#include "include/tvgpu.h"
#include "include/trace.h"
#include "vidpn.tmh"

static VOID
FillGraphicsMode(_Out_ D3DKMDT_VIDPN_SOURCE_MODE *Mode, _In_ UINT32 Width, _In_ UINT32 Height)
{
    RtlZeroMemory(Mode, sizeof(*Mode));
    Mode->Type = D3DKMDT_RMT_GRAPHICS;
    Mode->Format.Graphics.PrimSurfSize.cx = Width;
    Mode->Format.Graphics.PrimSurfSize.cy = Height;
    Mode->Format.Graphics.VisibleFromActive.cx = Width;
    Mode->Format.Graphics.VisibleFromActive.cy = Height;
    Mode->Format.Graphics.Stride = Width * 4;
    Mode->Format.Graphics.PixelFormat = D3DDDIFMT_A8R8G8B8;
    Mode->Format.Graphics.ColorBasis = D3DKMDT_CB_SCRGB;
    Mode->Format.Graphics.PixelValueAccessMode = D3DKMDT_PVAM_DIRECT;
}

static VOID
FillTargetMode(_Out_ D3DKMDT_VIDPN_TARGET_MODE *Mode, _In_ UINT32 Width, _In_ UINT32 Height)
{
    RtlZeroMemory(Mode, sizeof(*Mode));
    Mode->VideoSignalInfo.VideoStandard = D3DKMDT_VSS_OTHER;
    Mode->VideoSignalInfo.TotalSize.cx = Width;
    Mode->VideoSignalInfo.TotalSize.cy = Height;
    Mode->VideoSignalInfo.ActiveSize.cx = Width;
    Mode->VideoSignalInfo.ActiveSize.cy = Height;
    Mode->VideoSignalInfo.VSyncFreq.Numerator = 60;
    Mode->VideoSignalInfo.VSyncFreq.Denominator = 1;
    Mode->VideoSignalInfo.HSyncFreq.Numerator = Width * 60;
    Mode->VideoSignalInfo.HSyncFreq.Denominator = 1;
    Mode->VideoSignalInfo.PixelClock = (D3DKMDT_PIXEL_CLOCK)Width * Height * 60;
    Mode->VideoSignalInfo.ScanLineOrdering = D3DDDI_VSSLO_PROGRESSIVE;
    Mode->Preference = D3DKMDT_MP_PREFERRED;
}

NTSTATUS
TvgpuIsSupportedVidPn(_In_ PVOID MiniportDeviceContext, _Inout_ DXGKARG_ISSUPPORTEDVIDPN *Args)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    const DXGK_VIDPN_INTERFACE *vidpn = NULL;
    const DXGK_VIDPNTOPOLOGY_INTERFACE *topo = NULL;
    D3DKMDT_HVIDPNTOPOLOGY hTopo = 0;
    NTSTATUS status;
    SIZE_T paths = 0;
    SIZE_T i;

    if (dev == NULL || Args == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    Args->IsVidPnSupported = FALSE;
    if (Args->hDesiredVidPn == 0) {
        Args->IsVidPnSupported = TRUE;
        return STATUS_SUCCESS;
    }
    status = dev->Dxgk.DxgkCbQueryVidPnInterface(
        Args->hDesiredVidPn, DXGK_VIDPN_INTERFACE_VERSION_V1, &vidpn);
    if (!NT_SUCCESS(status) || vidpn == NULL) {
        return status;
    }
    status = vidpn->pfnGetTopology(Args->hDesiredVidPn, &hTopo, &topo);
    if (!NT_SUCCESS(status) || topo == NULL) {
        return status;
    }
    status = topo->pfnGetNumPaths(hTopo, &paths);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (paths > dev->NumScanouts) {
        TraceEvents(TRACE_LEVEL_WARNING, TRACE_VIDPN, "VidPN paths %Iu > scanouts", paths);
        return STATUS_SUCCESS;
    }
    for (i = 0; i < paths; i++) {
        D3DKMDT_VIDPN_PRESENT_PATH path;
        RtlZeroMemory(&path, sizeof(path));
        /* Path ids must match a host scanout. */
        UNREFERENCED_PARAMETER(path);
    }
    Args->IsVidPnSupported = TRUE;
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuRecommendFunctionalVidPn(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_RECOMMENDFUNCTIONALVIDPN *CONST Args)
{
    UNREFERENCED_PARAMETER(MiniportDeviceContext);
    UNREFERENCED_PARAMETER(Args);
    /* Let dxgkrnl enumerate cofunctional modality from our modes. */
    return STATUS_GRAPHICS_NO_RECOMMENDED_FUNCTIONAL_VIDPN;
}

static NTSTATUS
AddSourceMode(
    _In_ CONST DXGK_VIDPNSOURCEMODESET_INTERFACE *If,
    _In_ D3DKMDT_HVIDPNSOURCEMODESET Set,
    _In_ UINT32 Width,
    _In_ UINT32 Height)
{
    D3DKMDT_VIDPN_SOURCE_MODE *mode = NULL;
    NTSTATUS status = If->pfnCreateNewModeInfo(Set, &mode);
    if (!NT_SUCCESS(status) || mode == NULL) {
        return status;
    }
    FillGraphicsMode(mode, Width, Height);
    status = If->pfnAddMode(Set, mode);
    if (!NT_SUCCESS(status)) {
        (VOID)If->pfnReleaseModeInfo(Set, mode);
    }
    return status;
}

static NTSTATUS
AddTargetMode(
    _In_ CONST DXGK_VIDPNTARGETMODESET_INTERFACE *If,
    _In_ D3DKMDT_HVIDPNTARGETMODESET Set,
    _In_ UINT32 Width,
    _In_ UINT32 Height)
{
    D3DKMDT_VIDPN_TARGET_MODE *mode = NULL;
    NTSTATUS status = If->pfnCreateNewModeInfo(Set, &mode);
    if (!NT_SUCCESS(status) || mode == NULL) {
        return status;
    }
    FillTargetMode(mode, Width, Height);
    status = If->pfnAddMode(Set, mode);
    if (!NT_SUCCESS(status)) {
        (VOID)If->pfnReleaseModeInfo(Set, mode);
    }
    return status;
}

NTSTATUS
TvgpuEnumVidPnCofuncModality(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_ENUMVIDPNCOFUNCMODALITY *CONST Args)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    const DXGK_VIDPN_INTERFACE *vidpn = NULL;
    const DXGK_VIDPNTOPOLOGY_INTERFACE *topo = NULL;
    D3DKMDT_HVIDPNTOPOLOGY hTopo = 0;
    const D3DKMDT_VIDPN_PRESENT_PATH *path = NULL;
    NTSTATUS status;

    if (dev == NULL || Args == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    status = dev->Dxgk.DxgkCbQueryVidPnInterface(
        Args->hConstrainingVidPn, DXGK_VIDPN_INTERFACE_VERSION_V1, &vidpn);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = vidpn->pfnGetTopology(Args->hConstrainingVidPn, &hTopo, &topo);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = topo->pfnAcquireFirstPathInfo(hTopo, &path);
    while (NT_SUCCESS(status) && path != NULL) {
        UINT32 id = path->VidPnSourceId;
        UINT32 w;
        UINT32 h;
        D3DKMDT_HVIDPNSOURCEMODESET hSms = 0;
        const DXGK_VIDPNSOURCEMODESET_INTERFACE *sms = NULL;
        D3DKMDT_HVIDPNTARGETMODESET hTms = 0;
        const DXGK_VIDPNTARGETMODESET_INTERFACE *tms = NULL;
        CONST D3DKMDT_VIDPN_SOURCE_MODE *pinnedSrc = NULL;
        CONST D3DKMDT_VIDPN_TARGET_MODE *pinnedTgt = NULL;

        if (!NT_SUCCESS(TvgpuValidateScanoutId(dev, id))) {
            (VOID)topo->pfnReleasePathInfo(hTopo, path);
            return STATUS_GRAPHICS_INVALID_VIDEO_PRESENT_SOURCE;
        }
        w = dev->Scanouts[id].Width;
        h = dev->Scanouts[id].Height;
        if (w == 0 || h == 0) {
            w = 1280;
            h = 800;
        }

        status = vidpn->pfnAcquireSourceModeSet(
            Args->hConstrainingVidPn, path->VidPnSourceId, &hSms, &sms);
        if (NT_SUCCESS(status) && sms != NULL) {
            (VOID)sms->pfnAcquirePinnedModeInfo(hSms, &pinnedSrc);
            if (pinnedSrc == NULL &&
                Args->EnumPivotType != D3DKMDT_EPT_VIDPNSOURCE) {
                (VOID)AddSourceMode(sms, hSms, w, h);
            }
            if (pinnedSrc != NULL) {
                (VOID)sms->pfnReleaseModeInfo(hSms, pinnedSrc);
            }
            (VOID)vidpn->pfnReleaseSourceModeSet(Args->hConstrainingVidPn, hSms);
        }

        status = vidpn->pfnAcquireTargetModeSet(
            Args->hConstrainingVidPn, path->VidPnTargetId, &hTms, &tms);
        if (NT_SUCCESS(status) && tms != NULL) {
            (VOID)tms->pfnAcquirePinnedModeInfo(hTms, &pinnedTgt);
            if (pinnedTgt == NULL &&
                Args->EnumPivotType != D3DKMDT_EPT_VIDPNTARGET) {
                (VOID)AddTargetMode(tms, hTms, w, h);
            }
            if (pinnedTgt != NULL) {
                (VOID)tms->pfnReleaseModeInfo(hTms, pinnedTgt);
            }
            (VOID)vidpn->pfnReleaseTargetModeSet(Args->hConstrainingVidPn, hTms);
        }

        {
            const D3DKMDT_VIDPN_PRESENT_PATH *next = NULL;
            NTSTATUS n = topo->pfnAcquireNextPathInfo(hTopo, path, &next);
            (VOID)topo->pfnReleasePathInfo(hTopo, path);
            path = next;
            status = (n == STATUS_GRAPHICS_NO_MORE_ELEMENTS_IN_DATASET) ? STATUS_SUCCESS : n;
            if (next == NULL) {
                break;
            }
        }
    }
    TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_VIDPN, "EnumVidPnCofuncModality done");
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuCommitVidPn(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_COMMITVIDPN *CONST Args)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    const DXGK_VIDPN_INTERFACE *vidpn = NULL;
    const DXGK_VIDPNTOPOLOGY_INTERFACE *topo = NULL;
    D3DKMDT_HVIDPNTOPOLOGY hTopo = 0;
    const D3DKMDT_VIDPN_PRESENT_PATH *path = NULL;
    NTSTATUS status;

    if (dev == NULL || Args == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (Args->Flags.PathPowerTransition || Args->hFunctionalVidPn == 0) {
        return STATUS_SUCCESS;
    }
    status = dev->Dxgk.DxgkCbQueryVidPnInterface(
        Args->hFunctionalVidPn, DXGK_VIDPN_INTERFACE_VERSION_V1, &vidpn);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = vidpn->pfnGetTopology(Args->hFunctionalVidPn, &hTopo, &topo);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = topo->pfnAcquireFirstPathInfo(hTopo, &path);
    while (NT_SUCCESS(status) && path != NULL) {
        D3DKMDT_HVIDPNSOURCEMODESET hSms = 0;
        const DXGK_VIDPNSOURCEMODESET_INTERFACE *sms = NULL;
        CONST D3DKMDT_VIDPN_SOURCE_MODE *pinned = NULL;
        UINT32 id = path->VidPnSourceId;
        if (NT_SUCCESS(TvgpuValidateScanoutId(dev, id))) {
            status = vidpn->pfnAcquireSourceModeSet(
                Args->hFunctionalVidPn, id, &hSms, &sms);
            if (NT_SUCCESS(status) && sms != NULL) {
                (VOID)sms->pfnAcquirePinnedModeInfo(hSms, &pinned);
                if (pinned != NULL && pinned->Type == D3DKMDT_RMT_GRAPHICS) {
                    UINT32 w = pinned->Format.Graphics.PrimSurfSize.cx;
                    UINT32 h = pinned->Format.Graphics.PrimSurfSize.cy;
                    if (NT_SUCCESS(TvgpuValidateDimension(w, h))) {
                        dev->Scanouts[id].Width = w;
                        dev->Scanouts[id].Height = h;
                        TraceEvents(
                            TRACE_LEVEL_INFORMATION,
                            TRACE_VIDPN,
                            "commit src=%u %ux%u",
                            id,
                            w,
                            h);
                    }
                    (VOID)sms->pfnReleaseModeInfo(hSms, pinned);
                }
                (VOID)vidpn->pfnReleaseSourceModeSet(Args->hFunctionalVidPn, hSms);
            }
        }
        {
            const D3DKMDT_VIDPN_PRESENT_PATH *next = NULL;
            NTSTATUS n = topo->pfnAcquireNextPathInfo(hTopo, path, &next);
            (VOID)topo->pfnReleasePathInfo(hTopo, path);
            path = next;
            status = (n == STATUS_GRAPHICS_NO_MORE_ELEMENTS_IN_DATASET) ? STATUS_SUCCESS : n;
            if (next == NULL) {
                break;
            }
        }
    }
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuUpdateActiveVidPnPresentPath(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_UPDATEACTIVEVIDPNPRESENTPATH *CONST Args)
{
    UNREFERENCED_PARAMETER(MiniportDeviceContext);
    UNREFERENCED_PARAMETER(Args);
    return STATUS_SUCCESS;
}

NTSTATUS
TvgpuRecommendMonitorModes(
    _In_ PVOID MiniportDeviceContext,
    _In_ CONST DXGKARG_RECOMMENDMONITORMODES *CONST Args)
{
    TVGPU_DEVICE *dev = (TVGPU_DEVICE *)MiniportDeviceContext;
    D3DKMDT_MONITOR_SOURCE_MODE *mode = NULL;
    NTSTATUS status;
    UINT32 w;
    UINT32 h;
    UINT32 id;

    if (dev == NULL || Args == NULL || Args->pMonitorSourceModeSetInterface == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    id = Args->VideoPresentTargetId;
    if (!NT_SUCCESS(TvgpuValidateScanoutId(dev, id))) {
        return STATUS_GRAPHICS_INVALID_VIDEO_PRESENT_TARGET;
    }
    w = dev->Scanouts[id].Width;
    h = dev->Scanouts[id].Height;
    if (w == 0 || h == 0) {
        w = 1280;
        h = 800;
    }
    status = Args->pMonitorSourceModeSetInterface->pfnCreateNewModeInfo(
        Args->hMonitorSourceModeSet, &mode);
    if (!NT_SUCCESS(status) || mode == NULL) {
        return status;
    }
    RtlZeroMemory(mode, sizeof(*mode));
    mode->VideoSignalInfo.VideoStandard = D3DKMDT_VSS_OTHER;
    mode->VideoSignalInfo.TotalSize.cx = w;
    mode->VideoSignalInfo.TotalSize.cy = h;
    mode->VideoSignalInfo.ActiveSize.cx = w;
    mode->VideoSignalInfo.ActiveSize.cy = h;
    mode->VideoSignalInfo.VSyncFreq.Numerator = 60;
    mode->VideoSignalInfo.VSyncFreq.Denominator = 1;
    mode->VideoSignalInfo.HSyncFreq.Numerator = w * 60;
    mode->VideoSignalInfo.HSyncFreq.Denominator = 1;
    mode->VideoSignalInfo.PixelClock = (D3DKMDT_PIXEL_CLOCK)w * h * 60;
    mode->VideoSignalInfo.ScanLineOrdering = D3DDDI_VSSLO_PROGRESSIVE;
    mode->ColorBasis = D3DKMDT_CB_SRGB;
    mode->PixelValueAccessMode = D3DKMDT_PVAM_DIRECT;
    mode->Origin = D3DKMDT_MCO_DRIVER;
    mode->Preference = D3DKMDT_MP_PREFERRED;
    status = Args->pMonitorSourceModeSetInterface->pfnAddMode(Args->hMonitorSourceModeSet, mode);
    if (!NT_SUCCESS(status)) {
        (VOID)Args->pMonitorSourceModeSetInterface->pfnReleaseModeInfo(
            Args->hMonitorSourceModeSet, mode);
    }
    return status;
}

NTSTATUS
TvgpuQueryVidPnHWCapability(
    _In_ PVOID MiniportDeviceContext,
    _Inout_ DXGKARG_QUERYVIDPNHWCAPABILITY *Args)
{
    UNREFERENCED_PARAMETER(MiniportDeviceContext);
    if (Args == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    RtlZeroMemory(&Args->VidPnHWCaps, sizeof(Args->VidPnHWCaps));
    Args->VidPnHWCaps.DriverRotation = FALSE;
    Args->VidPnHWCaps.DriverScaling = FALSE;
    return STATUS_SUCCESS;
}
