/* Load tvvk.dll without the Vulkan loader. Expect 0 devices until TVGPU3D exists. */
#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#include <stdio.h>
#include <stdint.h>

typedef int32_t VkResult;
typedef void *VkInstance;
typedef void *(*PFN)(void);
typedef VkResult (*PFN_neg)(uint32_t *);
typedef PFN (*PFN_gipa)(VkInstance, const char *);
typedef size_t (*PFN_nop)(uint8_t *, size_t);

int main(void)
{
    HMODULE dll;
    PFN_neg neg;
    PFN_gipa gipa;
    PFN_nop nop;
    uint32_t ver = 5;
    uint8_t cs[8];
    size_t n;

    dll = LoadLibraryW(L"tvvk.dll");
    if (!dll) {
        printf("tvvk-smoke: LoadLibrary tvvk.dll failed (expected if not installed)\n");
        return 2;
    }
    neg = (PFN_neg)GetProcAddress(dll, "vk_icdNegotiateLoaderICDInterfaceVersion");
    gipa = (PFN_gipa)GetProcAddress(dll, "vk_icdGetInstanceProcAddr");
    nop = (PFN_nop)GetProcAddress(dll, "tvvk_export_nop");
    if (!neg || !gipa || !nop) {
        printf("tvvk-smoke: missing ICD exports\n");
        FreeLibrary(dll);
        return 1;
    }
    if (neg(&ver) != 0) {
        printf("tvvk-smoke: negotiate failed\n");
        FreeLibrary(dll);
        return 1;
    }
    n = nop(cs, sizeof(cs));
    if (n != 8 || cs[0] != 1 || cs[4] != 8) {
        printf("tvvk-smoke: nop CS mismatch\n");
        FreeLibrary(dll);
        return 1;
    }
    printf("tvvk-smoke: icd ok version=%u nop=ok\n", ver);
    printf("tvvk-smoke: vkcube WSI not implemented; display fallback is tvgpu-dod\n");
    FreeLibrary(dll);
    return 0;
}
#else
int main(void)
{
    return 0;
}
#endif
