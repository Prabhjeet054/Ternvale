#include "include/tvgpu.h"
#include "include/trace.h"
#include "driver.tmh"

VOID
TvgpuUnload(VOID)
{
    PDRIVER_OBJECT unused = NULL;
    UNREFERENCED_PARAMETER(unused);
    TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DRIVER, "Unload");
    WPP_CLEANUP(NULL);
}

NTSTATUS
DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath)
{
    DRIVER_INITIALIZATION_DATA init;
    NTSTATUS status;

    WPP_INIT_TRACING(DriverObject, RegistryPath);
    RtlZeroMemory(&init, sizeof(init));
    /* Display-only WDDM 1.3 miniport. TODO(verify): WDK version vs WDDM 2.x fields. */
    init.Version = DXGKDDI_INTERFACE_VERSION_WDDM1_3;
    init.DxgkDdiAddDevice = TvgpuAddDevice;
    init.DxgkDdiStartDevice = TvgpuStartDevice;
    init.DxgkDdiStopDevice = TvgpuStopDevice;
    init.DxgkDdiRemoveDevice = TvgpuRemoveDevice;
    init.DxgkDdiDispatchIoRequest = TvgpuDispatchIoRequest;
    init.DxgkDdiInterruptRoutine = TvgpuInterruptRoutine;
    init.DxgkDdiDpcRoutine = TvgpuDpcRoutine;
    init.DxgkDdiQueryChildRelations = TvgpuQueryChildRelations;
    init.DxgkDdiQueryChildStatus = TvgpuQueryChildStatus;
    init.DxgkDdiQueryDeviceDescriptor = TvgpuQueryDeviceDescriptor;
    init.DxgkDdiSetPowerState = TvgpuSetPowerState;
    init.DxgkDdiNotifyAcpiEvent = TvgpuNotifyAcpiEvent;
    init.DxgkDdiResetDevice = TvgpuResetDevice;
    init.DxgkDdiUnload = TvgpuUnload;
    init.DxgkDdiQueryAdapterInfo = TvgpuQueryAdapterInfo;
    init.DxgkDdiSetPointerPosition = TvgpuSetPointerPosition;
    init.DxgkDdiSetPointerShape = TvgpuSetPointerShape;
    init.DxgkDdiIsSupportedVidPn = TvgpuIsSupportedVidPn;
    init.DxgkDdiRecommendFunctionalVidPn = TvgpuRecommendFunctionalVidPn;
    init.DxgkDdiEnumVidPnCofuncModality = TvgpuEnumVidPnCofuncModality;
    init.DxgkDdiSetVidPnSourceAddress = TvgpuSetVidPnSourceAddress;
    init.DxgkDdiSetVidPnSourceVisibility = TvgpuSetVidPnSourceVisibility;
    init.DxgkDdiCommitVidPn = TvgpuCommitVidPn;
    init.DxgkDdiUpdateActiveVidPnPresentPath = TvgpuUpdateActiveVidPnPresentPath;
    init.DxgkDdiRecommendMonitorModes = TvgpuRecommendMonitorModes;
    init.DxgkDdiGetScanLine = TvgpuGetScanLine;
    init.DxgkDdiQueryVidPnHWCapability = TvgpuQueryVidPnHWCapability;
    init.DxgkDdiStopDeviceAndReleasePostDisplayOwnership =
        TvgpuStopDeviceAndReleasePostDisplayOwnership;

    status = DxgkInitialize(DriverObject, RegistryPath, &init);
    if (!NT_SUCCESS(status)) {
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_DRIVER, "DxgkInitialize %#x", status);
        WPP_CLEANUP(DriverObject);
        return status;
    }
    TvgpuAppendLog("DriverEntry");
    TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DRIVER, "DriverEntry ok");
    return status;
}
