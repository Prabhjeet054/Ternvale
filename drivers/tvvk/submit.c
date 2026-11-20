#include "submit.h"
#include "../include/tvgpu3d_ioctl.h"

#ifdef _WIN32

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

static HANDLE g_dev = INVALID_HANDLE_VALUE;

void *tvvk_kernel_handle(void)
{
    if (g_dev != INVALID_HANDLE_VALUE) {
        return (void *)g_dev;
    }
    g_dev = CreateFileW(
        TVGPU3D_WIN32_NAME,
        GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        NULL,
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL,
        NULL);
    return g_dev == INVALID_HANDLE_VALUE ? NULL : (void *)g_dev;
}

void tvvk_kernel_close(void)
{
    if (g_dev != INVALID_HANDLE_VALUE) {
        CloseHandle(g_dev);
        g_dev = INVALID_HANDLE_VALUE;
    }
}

int tvvk_kernel_submit(const uint8_t *cs, size_t len)
{
    HANDLE dev;
    DWORD ret = 0;
    BOOL ok;
    uint32_t ping = 0;

    if (cs == NULL || len == 0 || len > TVGPU3D_MAX_SUBMIT) {
        return 0;
    }
    dev = (HANDLE)tvvk_kernel_handle();
    if (dev == NULL) {
        return 0;
    }
    ok = DeviceIoControl(
        dev,
        IOCTL_TVGPU3D_PING,
        NULL,
        0,
        &ping,
        sizeof(ping),
        &ret,
        NULL);
    if (!ok || ping == 0) {
        return 0;
    }
    ok = DeviceIoControl(
        dev,
        IOCTL_TVGPU3D_SUBMIT,
        (LPVOID)cs,
        (DWORD)len,
        NULL,
        0,
        &ret,
        NULL);
    return ok ? 1 : 0;
}

#else

int tvvk_kernel_submit(const uint8_t *cs, size_t len)
{
    (void)cs;
    (void)len;
    return 0;
}

void *tvvk_kernel_handle(void)
{
    return NULL;
}

void tvvk_kernel_close(void) {}

#endif
