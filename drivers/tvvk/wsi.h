/* Win32 WSI for vkcube.exe. Present submits DRAW_CUBE to the host. */
#pragma once

#include "vk_min.h"

PFN_vkVoidFunction tvvk_wsi_proc(const char *name);
void tvvk_wsi_tick_present(void);
VkResult tvvk_EnumerateInstanceWsi(
    const char *layer,
    uint32_t *count,
    VkExtensionProperties *out);
VkResult tvvk_EnumerateDeviceWsi(
    VkPhysicalDevice pd,
    const char *layer,
    uint32_t *count,
    VkExtensionProperties *out);
