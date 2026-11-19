/* Device interface GUID and IOCTLs for the Ternvale KMDF hello driver.
 * {C7A1E4D2-9B3F-4A18-8E2C-1F5D6A7B8C90}
 */
#pragma once

#ifdef _KERNEL_MODE
#include <ntddk.h>
#else
#include <windows.h>
#include <winioctl.h>
#endif
#include <guiddef.h>

DEFINE_GUID(
    GUID_DEVINTERFACE_TVHELLO,
    0xC7A1E4D2,
    0x9B3F,
    0x4A18,
    0x8E,
    0x2C,
    0x1F,
    0x5D,
    0x6A,
    0x7B,
    0x8C,
    0x90);

#define IOCTL_TVHELLO_QUERY \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_TVHELLO_BUGCHECK \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_ANY_ACCESS)

#define TVHELLO_QUERY_MAGIC 0x30787674 /* 'tvh0' little-endian */

typedef struct _TVHELLO_QUERY {
    ULONG Magic;
    ULONG Version;
    ULONG D0Count;
} TVHELLO_QUERY, *PTVHELLO_QUERY;
