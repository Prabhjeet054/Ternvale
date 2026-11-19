/* PASSIVE_LEVEL sidecar under %ProgramData%\Ternvale\driver-logs.
 * The guest agent concatenates *.log here for `ternvale driver-logs`.
 * WPP still goes to ETW / WinDbg; this file is for host pull without KD.
 */

#include "device.h"

#include <ntstrsafe.h>

#define TVHELLO_LOG_DIR L"\\??\\C:\\ProgramData\\Ternvale\\driver-logs"
#define TVHELLO_LOG_PATH L"\\??\\C:\\ProgramData\\Ternvale\\driver-logs\\tvhello.log"

static NTSTATUS
EnsureLogDir(VOID)
{
    UNICODE_STRING dir;
    OBJECT_ATTRIBUTES attr;
    IO_STATUS_BLOCK iosb;
    HANDLE handle = NULL;
    NTSTATUS status;

    RtlInitUnicodeString(&dir, TVHELLO_LOG_DIR);
    InitializeObjectAttributes(
        &attr,
        &dir,
        OBJ_CASE_INSENSITIVE | OBJ_KERNEL_HANDLE,
        NULL,
        NULL);

    status = ZwCreateFile(
        &handle,
        FILE_LIST_DIRECTORY | SYNCHRONIZE,
        &attr,
        &iosb,
        NULL,
        FILE_ATTRIBUTE_DIRECTORY,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        FILE_OPEN_IF,
        FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
        NULL,
        0);
    if (NT_SUCCESS(status) && handle != NULL) {
        ZwClose(handle);
    }
    return status;
}

NTSTATUS
TvHelloAppendLog(_In_ PCSTR Message)
{
    UNICODE_STRING path;
    OBJECT_ATTRIBUTES attr;
    IO_STATUS_BLOCK iosb;
    HANDLE handle = NULL;
    NTSTATUS status;
    LARGE_INTEGER eof;
    size_t len = 0;
    CHAR line[256];

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = EnsureLogDir();
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = RtlStringCbPrintfA(line, sizeof(line), "%s\r\n", Message);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = RtlStringCbLengthA(line, sizeof(line), &len);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    RtlInitUnicodeString(&path, TVHELLO_LOG_PATH);
    InitializeObjectAttributes(
        &attr,
        &path,
        OBJ_CASE_INSENSITIVE | OBJ_KERNEL_HANDLE,
        NULL,
        NULL);

    status = ZwCreateFile(
        &handle,
        FILE_APPEND_DATA | SYNCHRONIZE,
        &attr,
        &iosb,
        NULL,
        FILE_ATTRIBUTE_NORMAL,
        FILE_SHARE_READ,
        FILE_OPEN_IF,
        FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
        NULL,
        0);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    eof.QuadPart = -1;
    status = ZwWriteFile(handle, NULL, NULL, NULL, &iosb, line, (ULONG)len, &eof, NULL);
    ZwClose(handle);
    return status;
}
