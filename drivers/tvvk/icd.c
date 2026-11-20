/*
 * Ternvale prototype Vulkan ICD (`tvvk.dll`). Independent implementation.
 * Not derived from Mesa Venus, Yttrium, or gfxstream.
 *
 * Loader ABI: vk_icdNegotiateLoaderICDInterfaceVersion + vk_icdGetInstanceProcAddr.
 * Win32 WSI is VK_ERROR_FEATURE_NOT_PRESENT (vkcube cannot present).
 * Physical devices: zero when probe fails (display-only fallback).
 */

#include "encode.h"
#include "probe.h"
#include "vk_min.h"

#include <string.h>

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#endif

struct VkInstance_T {
    uint32_t magic;
};

struct VkDevice_T {
    uint32_t magic;
};

struct VkPhysicalDevice_T {
    uint32_t magic;
};

#define TVVK_MAGIC 0x5456564bu /* 'TVVK' */
#define TVVK_ICD_VERSION 5u

static struct VkInstance_T g_instance;
static struct VkPhysicalDevice_T g_phys;
static struct VkDevice_T g_device;
static struct VkQueue_T *g_queue = (struct VkQueue_T *)(uintptr_t)0x54565155u;

static int streq(const char *a, const char *b)
{
    if (a == NULL || b == NULL) {
        return 0;
    }
    return strcmp(a, b) == 0;
}

static void fill_props(VkPhysicalDeviceProperties *p)
{
    static const char name[] = "Ternvale Venus prototype";
    memset(p, 0, sizeof(*p));
    p->apiVersion = VK_API_VERSION_1_0;
    p->driverVersion = 1;
    p->vendorID = 0x1af4;
    p->deviceID = 0x1050;
    p->deviceType = VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU;
    memcpy(p->deviceName, name, sizeof(name));
    p->limits.maxImageDimension1D = 4096;
    p->limits.maxImageDimension2D = 4096;
}

VKAPI_ATTR VkResult VKAPI_CALL vkCreateInstance(
    const VkInstanceCreateInfo *info,
    const VkAllocationCallbacks *alloc,
    VkInstance *out)
{
    (void)info;
    (void)alloc;
    if (out == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    g_instance.magic = TVVK_MAGIC;
    *out = &g_instance;
    return VK_SUCCESS;
}

VKAPI_ATTR void VKAPI_CALL vkDestroyInstance(VkInstance inst, const VkAllocationCallbacks *alloc)
{
    (void)alloc;
    if (inst != NULL) {
        inst->magic = 0;
    }
}

VKAPI_ATTR VkResult VKAPI_CALL vkEnumeratePhysicalDevices(
    VkInstance inst,
    uint32_t *count,
    VkPhysicalDevice *out)
{
    uint32_t n;
    if (inst == NULL || count == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    n = tvvk_gpu3d_enabled() ? 1u : 0u;
    if (out == NULL) {
        *count = n;
        return VK_SUCCESS;
    }
    if (*count < n) {
        *count = n;
        return VK_INCOMPLETE;
    }
    if (n == 1) {
        g_phys.magic = TVVK_MAGIC;
        out[0] = &g_phys;
    }
    *count = n;
    return VK_SUCCESS;
}

VKAPI_ATTR void VKAPI_CALL vkGetPhysicalDeviceProperties(
    VkPhysicalDevice pd,
    VkPhysicalDeviceProperties *props)
{
    (void)pd;
    if (props != NULL) {
        fill_props(props);
    }
}

VKAPI_ATTR void VKAPI_CALL vkGetPhysicalDeviceFeatures(
    VkPhysicalDevice pd,
    VkPhysicalDeviceFeatures *feat)
{
    (void)pd;
    if (feat != NULL) {
        memset(feat, 0, sizeof(*feat));
    }
}

VKAPI_ATTR void VKAPI_CALL vkGetPhysicalDeviceQueueFamilyProperties(
    VkPhysicalDevice pd,
    uint32_t *count,
    VkQueueFamilyProperties *out)
{
    (void)pd;
    if (count == NULL) {
        return;
    }
    if (out == NULL) {
        *count = 1;
        return;
    }
    if (*count < 1) {
        *count = 1;
        return;
    }
    memset(&out[0], 0, sizeof(out[0]));
    out[0].queueFlags = VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_COMPUTE_BIT;
    out[0].queueCount = 1;
    *count = 1;
}

VKAPI_ATTR void VKAPI_CALL vkGetPhysicalDeviceMemoryProperties(
    VkPhysicalDevice pd,
    VkPhysicalDeviceMemoryProperties *mem)
{
    (void)pd;
    if (mem == NULL) {
        return;
    }
    memset(mem, 0, sizeof(*mem));
    mem->memoryTypeCount = 1;
    mem->memoryTypes[0].propertyFlags = 0x00000003u; /* HOST_VISIBLE|HOST_COHERENT */
    mem->memoryHeapCount = 1;
    mem->memoryHeaps[0].size = 64u * 1024u * 1024u;
}

VKAPI_ATTR VkResult VKAPI_CALL vkCreateDevice(
    VkPhysicalDevice pd,
    const VkDeviceCreateInfo *info,
    const VkAllocationCallbacks *alloc,
    VkDevice *out)
{
    (void)pd;
    (void)info;
    (void)alloc;
    if (!tvvk_gpu3d_enabled() || out == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    g_device.magic = TVVK_MAGIC;
    *out = &g_device;
    return VK_SUCCESS;
}

VKAPI_ATTR void VKAPI_CALL vkDestroyDevice(VkDevice dev, const VkAllocationCallbacks *alloc)
{
    (void)alloc;
    if (dev != NULL) {
        dev->magic = 0;
    }
}

VKAPI_ATTR void VKAPI_CALL vkGetDeviceQueue(VkDevice dev, uint32_t family, uint32_t index, VkQueue *out)
{
    (void)dev;
    (void)family;
    (void)index;
    if (out != NULL) {
        *out = g_queue;
    }
}

VKAPI_ATTR VkResult VKAPI_CALL vkEnumerateInstanceExtensionProperties(
    const char *layer,
    uint32_t *count,
    VkExtensionProperties *out)
{
    (void)layer;
    (void)out;
    if (count == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    *count = 0;
    return VK_SUCCESS;
}

VKAPI_ATTR VkResult VKAPI_CALL vkEnumerateDeviceExtensionProperties(
    VkPhysicalDevice pd,
    const char *layer,
    uint32_t *count,
    VkExtensionProperties *out)
{
    (void)pd;
    (void)layer;
    (void)out;
    if (count == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    *count = 0;
    return VK_SUCCESS;
}

VKAPI_ATTR VkResult VKAPI_CALL vkEnumerateInstanceLayerProperties(
    uint32_t *count,
    VkLayerProperties *out)
{
    (void)out;
    if (count == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    *count = 0;
    return VK_SUCCESS;
}

VKAPI_ATTR VkResult VKAPI_CALL vkEnumerateInstanceVersion(uint32_t *version)
{
    if (version == NULL) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    *version = VK_API_VERSION_1_0;
    return VK_SUCCESS;
}

VKAPI_ATTR VkResult VKAPI_CALL vkCreateWin32SurfaceKHR(void *a, void *b, void *c, void *d)
{
    (void)a;
    (void)b;
    (void)c;
    (void)d;
    return VK_ERROR_FEATURE_NOT_PRESENT;
}

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vkGetInstanceProcAddr(VkInstance inst, const char *name);
VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vkGetDeviceProcAddr(VkDevice dev, const char *name);

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vkGetInstanceProcAddr(VkInstance inst, const char *name)
{
    (void)inst;
    if (streq(name, "vkCreateInstance")) {
        return (PFN_vkVoidFunction)vkCreateInstance;
    }
    if (streq(name, "vkDestroyInstance")) {
        return (PFN_vkVoidFunction)vkDestroyInstance;
    }
    if (streq(name, "vkEnumeratePhysicalDevices")) {
        return (PFN_vkVoidFunction)vkEnumeratePhysicalDevices;
    }
    if (streq(name, "vkGetPhysicalDeviceProperties")) {
        return (PFN_vkVoidFunction)vkGetPhysicalDeviceProperties;
    }
    if (streq(name, "vkGetPhysicalDeviceFeatures")) {
        return (PFN_vkVoidFunction)vkGetPhysicalDeviceFeatures;
    }
    if (streq(name, "vkGetPhysicalDeviceQueueFamilyProperties")) {
        return (PFN_vkVoidFunction)vkGetPhysicalDeviceQueueFamilyProperties;
    }
    if (streq(name, "vkGetPhysicalDeviceMemoryProperties")) {
        return (PFN_vkVoidFunction)vkGetPhysicalDeviceMemoryProperties;
    }
    if (streq(name, "vkCreateDevice")) {
        return (PFN_vkVoidFunction)vkCreateDevice;
    }
    if (streq(name, "vkDestroyDevice")) {
        return (PFN_vkVoidFunction)vkDestroyDevice;
    }
    if (streq(name, "vkGetDeviceQueue")) {
        return (PFN_vkVoidFunction)vkGetDeviceQueue;
    }
    if (streq(name, "vkEnumerateInstanceExtensionProperties")) {
        return (PFN_vkVoidFunction)vkEnumerateInstanceExtensionProperties;
    }
    if (streq(name, "vkEnumerateDeviceExtensionProperties")) {
        return (PFN_vkVoidFunction)vkEnumerateDeviceExtensionProperties;
    }
    if (streq(name, "vkEnumerateInstanceLayerProperties")) {
        return (PFN_vkVoidFunction)vkEnumerateInstanceLayerProperties;
    }
    if (streq(name, "vkEnumerateInstanceVersion")) {
        return (PFN_vkVoidFunction)vkEnumerateInstanceVersion;
    }
    if (streq(name, "vkGetInstanceProcAddr")) {
        return (PFN_vkVoidFunction)vkGetInstanceProcAddr;
    }
    if (streq(name, "vkGetDeviceProcAddr")) {
        return (PFN_vkVoidFunction)vkGetDeviceProcAddr;
    }
    if (streq(name, "vkCreateWin32SurfaceKHR")) {
        return (PFN_vkVoidFunction)vkCreateWin32SurfaceKHR;
    }
    return NULL;
}

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vkGetDeviceProcAddr(VkDevice dev, const char *name)
{
    (void)dev;
    if (streq(name, "vkGetDeviceProcAddr")) {
        return (PFN_vkVoidFunction)vkGetDeviceProcAddr;
    }
    if (streq(name, "vkDestroyDevice")) {
        return (PFN_vkVoidFunction)vkDestroyDevice;
    }
    if (streq(name, "vkGetDeviceQueue")) {
        return (PFN_vkVoidFunction)vkGetDeviceQueue;
    }
    return NULL;
}

#ifdef _WIN32
__declspec(dllexport)
#endif
VKAPI_ATTR VkResult VKAPI_CALL
vk_icdNegotiateLoaderICDInterfaceVersion(uint32_t *version)
{
    if (version == NULL) {
        return VK_ERROR_INCOMPATIBLE_DRIVER;
    }
    if (*version < 4) {
        return VK_ERROR_INCOMPATIBLE_DRIVER;
    }
    if (*version > TVVK_ICD_VERSION) {
        *version = TVVK_ICD_VERSION;
    }
    return VK_SUCCESS;
}

#ifdef _WIN32
__declspec(dllexport)
#endif
VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL
vk_icdGetInstanceProcAddr(VkInstance inst, const char *name)
{
    return vkGetInstanceProcAddr(inst, name);
}

#ifdef _WIN32
__declspec(dllexport)
#endif
VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL
vk_icdGetPhysicalDeviceProcAddr(VkInstance inst, const char *name)
{
    (void)inst;
    (void)name;
    return NULL;
}

#ifdef _WIN32
__declspec(dllexport)
#endif
size_t tvvk_export_nop(uint8_t *out, size_t cap)
{
    return tvvk_encode_nop(out, cap);
}
