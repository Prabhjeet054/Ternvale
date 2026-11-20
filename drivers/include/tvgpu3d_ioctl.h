/* Shared user/kernel IOCTLs for \\.\TVGPU3D (tvvk ICD → tvgpu-dod). */
#pragma once

#ifdef _KERNEL_MODE
#include <ntddk.h>
#else
#include <windows.h>
#include <winioctl.h>
#endif

#define TVGPU3D_DEVICE_NAME L"\\Device\\TVGPU3D"
#define TVGPU3D_SYMLINK_NAME L"\\DosDevices\\TVGPU3D"
#define TVGPU3D_WIN32_NAME L"\\\\.\\TVGPU3D"

#define IOCTL_TVGPU3D_PING \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0xA01, METHOD_BUFFERED, FILE_READ_ACCESS)
#define IOCTL_TVGPU3D_SUBMIT \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0xA02, METHOD_BUFFERED, FILE_WRITE_ACCESS)

#define TVGPU3D_MAX_SUBMIT 65536u
