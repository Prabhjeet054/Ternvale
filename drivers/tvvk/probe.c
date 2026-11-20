/*
 * Fail closed: no kernel 3D ioctl and no host gpu3d → zero devices.
 * tvgpu-dod (KMDOD) stays the display path. This file is Windows-only.
 */

#include "probe.h"

#ifdef _WIN32

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <stdio.h>
#include <windows.h>

/* Sidecar under ProgramData; no secrets. */
static void tvvk_log(const char *msg)
{
    HANDLE file;
    DWORD wrote;
    char line[256];
    int n;
    CreateDirectoryA("C:\\ProgramData\\Ternvale", NULL);
    CreateDirectoryA("C:\\ProgramData\\Ternvale\\driver-logs", NULL);
    file = CreateFileA(
        "C:\\ProgramData\\Ternvale\\driver-logs\\tvvk.log",
        FILE_APPEND_DATA,
        FILE_SHARE_READ,
        NULL,
        OPEN_ALWAYS,
        FILE_ATTRIBUTE_NORMAL,
        NULL);
    if (file == INVALID_HANDLE_VALUE) {
        return;
    }
    n = snprintf(line, sizeof(line), "tvvk: %s\r\n", msg);
    if (n > 0) {
        WriteFile(file, line, (DWORD)n, &wrote, NULL);
    }
    CloseHandle(file);
}

static int env_disabled(void)
{
    char buf[8];
    DWORD n = GetEnvironmentVariableA("TERNVALE_GPU3D", buf, sizeof(buf));
    if (n == 1 && (buf[0] == '0' || buf[0] == 'n' || buf[0] == 'N')) {
        return 1;
    }
    return 0;
}

static int registry_disabled(void)
{
    HKEY key;
    DWORD value = 1;
    DWORD size = sizeof(value);
    DWORD type = 0;
    if (RegOpenKeyExA(
            HKEY_LOCAL_MACHINE,
            "SOFTWARE\\Ternvale\\Gpu3d",
            0,
            KEY_READ,
            &key)
        != ERROR_SUCCESS) {
        return 0;
    }
    if (RegQueryValueExA(key, "Enabled", NULL, &type, (LPBYTE)&value, &size) == ERROR_SUCCESS
        && type == REG_DWORD && value == 0) {
        RegCloseKey(key);
        return 1;
    }
    RegCloseKey(key);
    return 0;
}

int tvvk_gpu3d_enabled(void)
{
    HANDLE dev;

    if (env_disabled()) {
        tvvk_log("disabled by TERNVALE_GPU3D");
        return 0;
    }
    if (registry_disabled()) {
        tvvk_log("disabled by HKLM\\SOFTWARE\\Ternvale\\Gpu3d");
        return 0;
    }

    /*
     * Kernel 3D path is not in tvgpu-dod (display-only). CreateFile fails
     * until a render IOCTL exists. Fail closed so the loader skips this ICD
     * and the KMDOD keeps scanout.
     */
    dev = CreateFileW(
        L"\\\\.\\TVGPU3D",
        GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        NULL,
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL,
        NULL);
    if (dev == INVALID_HANDLE_VALUE) {
        tvvk_log("3D init failed: \\\\.\\TVGPU3D missing; fallback to display-only");
        return 0;
    }
    CloseHandle(dev);
    tvvk_log("3D device node present (prototype)");
    return 1;
}

#else

int tvvk_gpu3d_enabled(void)
{
    return 0;
}

#endif
