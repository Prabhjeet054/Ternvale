/* Extra \\.\TVGPU3D control device beside the KMDOD (user-mode ICD). */

#include "include/tvgpu.h"
#include "include/trace.h"
#include "../include/tvgpu3d_ioctl.h"
#include "control.tmh"

static PDRIVER_DISPATCH g_OldCreate;
static PDRIVER_DISPATCH g_OldClose;
static PDRIVER_DISPATCH g_OldCleanup;
static PDRIVER_DISPATCH g_OldIoctl;
static DEVICE_OBJECT *g_Control;
static TVGPU_DEVICE *g_Gpu;
static BOOLEAN g_Hooked;

static NTSTATUS
Complete(_In_ IRP *Irp, _In_ NTSTATUS Status, _In_ ULONG_PTR Info)
{
    Irp->IoStatus.Status = Status;
    Irp->IoStatus.Information = Info;
    IoCompleteRequest(Irp, IO_NO_INCREMENT);
    return Status;
}

static NTSTATUS
ControlDispatch(_In_ DEVICE_OBJECT *DeviceObject, _In_ IRP *Irp)
{
    IO_STACK_LOCATION *stack;
    NTSTATUS status;
    UINT32 ping;
    ULONG code;
    UINT32 inLen;
    PVOID inBuf;

    if (DeviceObject != g_Control) {
        stack = IoGetCurrentIrpStackLocation(Irp);
        if (stack->MajorFunction == IRP_MJ_CREATE && g_OldCreate != NULL) {
            return g_OldCreate(DeviceObject, Irp);
        }
        if (stack->MajorFunction == IRP_MJ_CLOSE && g_OldClose != NULL) {
            return g_OldClose(DeviceObject, Irp);
        }
        if (stack->MajorFunction == IRP_MJ_CLEANUP && g_OldCleanup != NULL) {
            return g_OldCleanup(DeviceObject, Irp);
        }
        if (stack->MajorFunction == IRP_MJ_DEVICE_CONTROL && g_OldIoctl != NULL) {
            return g_OldIoctl(DeviceObject, Irp);
        }
        return Complete(Irp, STATUS_INVALID_DEVICE_REQUEST, 0);
    }

    stack = IoGetCurrentIrpStackLocation(Irp);
    if (stack->MajorFunction == IRP_MJ_CREATE || stack->MajorFunction == IRP_MJ_CLOSE
        || stack->MajorFunction == IRP_MJ_CLEANUP) {
        return Complete(Irp, STATUS_SUCCESS, 0);
    }
    if (stack->MajorFunction != IRP_MJ_DEVICE_CONTROL) {
        return Complete(Irp, STATUS_INVALID_DEVICE_REQUEST, 0);
    }

    code = stack->Parameters.DeviceIoControl.IoControlCode;
    if (code == IOCTL_TVGPU3D_PING) {
        ping = (g_Gpu != NULL && g_Gpu->ThreeD) ? 1u : 0u;
        if (stack->Parameters.DeviceIoControl.OutputBufferLength < sizeof(ping)
            || Irp->AssociatedIrp.SystemBuffer == NULL) {
            return Complete(Irp, STATUS_BUFFER_TOO_SMALL, 0);
        }
        RtlCopyMemory(Irp->AssociatedIrp.SystemBuffer, &ping, sizeof(ping));
        return Complete(Irp, STATUS_SUCCESS, sizeof(ping));
    }
    if (code != IOCTL_TVGPU3D_SUBMIT) {
        return Complete(Irp, STATUS_INVALID_DEVICE_REQUEST, 0);
    }
    if (g_Gpu == NULL) {
        return Complete(Irp, STATUS_DEVICE_NOT_READY, 0);
    }
    inLen = stack->Parameters.DeviceIoControl.InputBufferLength;
    inBuf = Irp->AssociatedIrp.SystemBuffer;
    if (inBuf == NULL || inLen == 0 || inLen > TVGPU3D_MAX_SUBMIT) {
        return Complete(Irp, STATUS_INVALID_PARAMETER, 0);
    }
    status = TvgpuSubmitUserCs(g_Gpu, inBuf, inLen);
    return Complete(Irp, status, 0);
}

VOID
TvgpuHookDriver(_In_ PDRIVER_OBJECT DriverObject)
{
    if (DriverObject == NULL || g_Hooked) {
        return;
    }
    g_OldCreate = DriverObject->MajorFunction[IRP_MJ_CREATE];
    g_OldClose = DriverObject->MajorFunction[IRP_MJ_CLOSE];
    g_OldCleanup = DriverObject->MajorFunction[IRP_MJ_CLEANUP];
    g_OldIoctl = DriverObject->MajorFunction[IRP_MJ_DEVICE_CONTROL];
    DriverObject->MajorFunction[IRP_MJ_CREATE] = ControlDispatch;
    DriverObject->MajorFunction[IRP_MJ_CLOSE] = ControlDispatch;
    DriverObject->MajorFunction[IRP_MJ_CLEANUP] = ControlDispatch;
    DriverObject->MajorFunction[IRP_MJ_DEVICE_CONTROL] = ControlDispatch;
    g_Hooked = TRUE;
    TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DRIVER, "hooked control IRPs for TVGPU3D");
}

NTSTATUS
TvgpuControlStart(_Inout_ TVGPU_DEVICE *Dev)
{
    UNICODE_STRING name;
    UNICODE_STRING link;
    NTSTATUS status;

    if (Dev == NULL || Dev->PhysicalDeviceObject == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    RtlInitUnicodeString(&name, TVGPU3D_DEVICE_NAME);
    RtlInitUnicodeString(&link, TVGPU3D_SYMLINK_NAME);
    status = IoCreateDevice(
        Dev->PhysicalDeviceObject->DriverObject,
        0,
        &name,
        FILE_DEVICE_UNKNOWN,
        FILE_DEVICE_SECURE_OPEN,
        FALSE,
        &Dev->ControlDevice);
    if (!NT_SUCCESS(status)) {
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_DRIVER, "IoCreateDevice TVGPU3D %#x", status);
        return status;
    }
    status = IoCreateSymbolicLink(&link, &name);
    if (!NT_SUCCESS(status)) {
        IoDeleteDevice(Dev->ControlDevice);
        Dev->ControlDevice = NULL;
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_DRIVER, "IoCreateSymbolicLink TVGPU3D %#x", status);
        return status;
    }
    Dev->ControlDevice->Flags &= ~DO_DEVICE_INITIALIZING;
    g_Control = Dev->ControlDevice;
    g_Gpu = Dev;
    TvgpuAppendLog("TVGPU3D control device ready");
    return STATUS_SUCCESS;
}

VOID
TvgpuControlStop(_Inout_ TVGPU_DEVICE *Dev)
{
    UNICODE_STRING link;
    if (Dev == NULL) {
        return;
    }
    if (g_Gpu == Dev) {
        g_Control = NULL;
        g_Gpu = NULL;
    }
    RtlInitUnicodeString(&link, TVGPU3D_SYMLINK_NAME);
    (VOID)IoDeleteSymbolicLink(&link);
    if (Dev->ControlDevice != NULL) {
        IoDeleteDevice(Dev->ControlDevice);
        Dev->ControlDevice = NULL;
    }
}
