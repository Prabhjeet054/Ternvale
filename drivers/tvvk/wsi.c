/*
 * Minimal VK_KHR_win32_surface + swapchain. Present sends DRAW_CUBE CS
 * through \\.\TVGPU3D and paints the HWND so vkcube is not a black window.
 */

#include "wsi.h"
#include "encode.h"
#include "submit.h"

#include <string.h>

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#endif

struct VkSurfaceKHR_T {
    uint32_t magic;
#ifdef _WIN32
    HWND hwnd;
#endif
};

struct VkSwapchainKHR_T {
    uint32_t magic;
    uint32_t width;
    uint32_t height;
#ifdef _WIN32
    HWND hwnd;
#endif
};

static struct VkSurfaceKHR_T g_surface;
static struct VkSwapchainKHR_T g_sc;
static uint32_t g_frame;

#ifdef _WIN32
static void blit_hwnd(HWND hwnd, uint32_t w, uint32_t h, uint32_t frame)
{
    HDC dc;
    RECT r;
    HBRUSH brush;
    COLORREF color;
    if (hwnd == NULL) {
        return;
    }
    dc = GetDC(hwnd);
    if (dc == NULL) {
        return;
    }
    if (w == 0) {
        w = 64;
    }
    if (h == 0) {
        h = 64;
    }
    GetClientRect(hwnd, &r);
    color = RGB((frame * 17u) & 255u, 80, 200);
    brush = CreateSolidBrush(color);
    FillRect(dc, &r, brush);
    DeleteObject(brush);
    ReleaseDC(hwnd, dc);
    (void)w;
    (void)h;
}
#endif

static int streq(const char *a, const char *b)
{
    return a != NULL && b != NULL && strcmp(a, b) == 0;
}

static void copy_ext(VkExtensionProperties *out, const char *name)
{
    memset(out, 0, sizeof(*out));
    strncpy(out->extensionName, name, VK_MAX_EXTENSION_NAME_SIZE - 1);
    out->specVersion = 1;
}

VKAPI_ATTR VkResult VKAPI_CALL tvvk_CreateWin32SurfaceKHR(
    VkInstance inst,
    const void *info,
    const VkAllocationCallbacks *alloc,
    VkSurfaceKHR *out)
{
#ifdef _WIN32
    const uint8_t *bytes = (const uint8_t *)info;
#endif
    (void)inst;
    (void)alloc;
    if (out == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    g_surface.magic = 0x53524643u;
#ifdef _WIN32
    /* VkWin32SurfaceCreateInfoKHR: sType, pNext, flags, hinstance, hwnd. */
    if (bytes != NULL) {
        memcpy(&g_surface.hwnd, bytes + 16 + sizeof(void *) + 4 + sizeof(void *), sizeof(HWND));
        /* TODO(verify): offset of hwnd vs SDK layout; fallback below. */
        if (g_surface.hwnd == NULL) {
            memcpy(&g_surface.hwnd, bytes + 24, sizeof(HWND));
        }
    }
#endif
    *out = &g_surface;
    return VK_SUCCESS;
}

VKAPI_ATTR void VKAPI_CALL tvvk_DestroySurfaceKHR(
    VkInstance inst,
    VkSurfaceKHR surface,
    const VkAllocationCallbacks *alloc)
{
    (void)inst;
    (void)surface;
    (void)alloc;
}

VKAPI_ATTR VkResult VKAPI_CALL tvvk_GetPhysicalDeviceSurfaceSupportKHR(
    VkPhysicalDevice pd,
    uint32_t family,
    VkSurfaceKHR surface,
    VkBool32 *ok)
{
    (void)pd;
    (void)family;
    (void)surface;
    if (ok == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    *ok = VK_TRUE;
    return VK_SUCCESS;
}

VKAPI_ATTR VkResult VKAPI_CALL tvvk_CreateSwapchainKHR(
    VkDevice dev,
    const void *info,
    const VkAllocationCallbacks *alloc,
    VkSwapchainKHR *out)
{
    (void)dev;
    (void)info;
    (void)alloc;
    if (out == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    g_sc.magic = 0x53574348u;
    g_sc.width = 800;
    g_sc.height = 600;
#ifdef _WIN32
    g_sc.hwnd = g_surface.hwnd;
#endif
    *out = &g_sc;
    return VK_SUCCESS;
}

VKAPI_ATTR void VKAPI_CALL tvvk_DestroySwapchainKHR(
    VkDevice dev,
    VkSwapchainKHR sc,
    const VkAllocationCallbacks *alloc)
{
    (void)dev;
    (void)sc;
    (void)alloc;
}

VKAPI_ATTR VkResult VKAPI_CALL tvvk_GetSwapchainImagesKHR(
    VkDevice dev,
    VkSwapchainKHR sc,
    uint32_t *count,
    VkImage *images)
{
    (void)dev;
    (void)sc;
    (void)images;
    if (count == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    if (images == NULL) {
        *count = 1;
        return VK_SUCCESS;
    }
    if (*count < 1) {
        *count = 1;
        return VK_INCOMPLETE;
    }
    images[0] = (VkImage)(uintptr_t)0x54564B49u;
    *count = 1;
    return VK_SUCCESS;
}

VKAPI_ATTR VkResult VKAPI_CALL tvvk_AcquireNextImageKHR(
    VkDevice dev,
    VkSwapchainKHR sc,
    uint64_t timeout,
    VkSemaphore sem,
    VkFence fence,
    uint32_t *index)
{
    (void)dev;
    (void)sc;
    (void)timeout;
    (void)sem;
    (void)fence;
    if (index == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    *index = 0;
    return VK_SUCCESS;
}

VKAPI_ATTR VkResult VKAPI_CALL tvvk_QueuePresentKHR(VkQueue queue, const void *info)
{
    uint8_t cs[32];
    size_t n;
    (void)queue;
    (void)info;
    g_frame++;
    n = tvvk_encode_draw_cube(cs, sizeof(cs), g_sc.width, g_sc.height, g_frame);
    if (n == 0 || !tvvk_kernel_submit(cs, n)) {
        return VK_ERROR_DEVICE_LOST;
    }
    n = tvvk_encode_present(cs, sizeof(cs));
    if (n != 0) {
        (void)tvvk_kernel_submit(cs, n);
    }
#ifdef _WIN32
    blit_hwnd(g_sc.hwnd, g_sc.width, g_sc.height, g_frame);
#endif
    return VK_SUCCESS;
}

void tvvk_wsi_tick_present(void)
{
    (void)tvvk_QueuePresentKHR(NULL, NULL);
}

VKAPI_ATTR VkResult VKAPI_CALL tvvk_EnumerateInstanceWsi(
    const char *layer,
    uint32_t *count,
    VkExtensionProperties *out)
{
    const char *names[2] = {"VK_KHR_surface", "VK_KHR_win32_surface"};
    uint32_t i;
    (void)layer;
    if (count == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    if (out == NULL) {
        *count = 2;
        return VK_SUCCESS;
    }
    if (*count < 2) {
        *count = 2;
        return VK_INCOMPLETE;
    }
    for (i = 0; i < 2; i++) {
        copy_ext(&out[i], names[i]);
    }
    *count = 2;
    return VK_SUCCESS;
}

VKAPI_ATTR VkResult VKAPI_CALL tvvk_EnumerateDeviceWsi(
    VkPhysicalDevice pd,
    const char *layer,
    uint32_t *count,
    VkExtensionProperties *out)
{
    (void)pd;
    (void)layer;
    if (count == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    if (out == NULL) {
        *count = 1;
        return VK_SUCCESS;
    }
    if (*count < 1) {
        *count = 1;
        return VK_INCOMPLETE;
    }
    copy_ext(&out[0], "VK_KHR_swapchain");
    *count = 1;
    return VK_SUCCESS;
}

PFN_vkVoidFunction tvvk_wsi_proc(const char *name)
{
    if (streq(name, "vkCreateWin32SurfaceKHR")) {
        return (PFN_vkVoidFunction)tvvk_CreateWin32SurfaceKHR;
    }
    if (streq(name, "vkDestroySurfaceKHR")) {
        return (PFN_vkVoidFunction)tvvk_DestroySurfaceKHR;
    }
    if (streq(name, "vkGetPhysicalDeviceSurfaceSupportKHR")) {
        return (PFN_vkVoidFunction)tvvk_GetPhysicalDeviceSurfaceSupportKHR;
    }
    if (streq(name, "vkCreateSwapchainKHR")) {
        return (PFN_vkVoidFunction)tvvk_CreateSwapchainKHR;
    }
    if (streq(name, "vkDestroySwapchainKHR")) {
        return (PFN_vkVoidFunction)tvvk_DestroySwapchainKHR;
    }
    if (streq(name, "vkGetSwapchainImagesKHR")) {
        return (PFN_vkVoidFunction)tvvk_GetSwapchainImagesKHR;
    }
    if (streq(name, "vkAcquireNextImageKHR")) {
        return (PFN_vkVoidFunction)tvvk_AcquireNextImageKHR;
    }
    if (streq(name, "vkQueuePresentKHR")) {
        return (PFN_vkVoidFunction)tvvk_QueuePresentKHR;
    }
    if (streq(name, "vkEnumerateInstanceExtensionProperties")) {
        return (PFN_vkVoidFunction)tvvk_EnumerateInstanceWsi;
    }
    if (streq(name, "vkEnumerateDeviceExtensionProperties")) {
        return (PFN_vkVoidFunction)tvvk_EnumerateDeviceWsi;
    }
    return NULL;
}
