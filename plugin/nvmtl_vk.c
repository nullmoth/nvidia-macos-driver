/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include <dlfcn.h>
#include <mach-o/loader.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdatomic.h>
#include <math.h>
#define VK_NO_PROTOTYPES 1
#include <vulkan/vulkan.h>
#include "nvmtl_vk.h"
#include "nvmtl_sampler_state.h"
static int g_sampler_anisotropy, g_sampler_mirror_clamp;
static int g_scalar_layout;

void nvlog(const char *fmt, ...);

static __thread uint32_t g_copy_aspect_override, g_copy_bpp_override;
void nvmtl_vk_copy_aspect_override(uint32_t aspect, uint32_t bpp) { g_copy_aspect_override = aspect; g_copy_bpp_override = bpp; }
static VkImageAspectFlags nvmtl_copy_aspect(const nvk_image *img)
{
    if (g_copy_aspect_override) return (VkImageAspectFlags)g_copy_aspect_override;
    switch (img ? img->fmt : 0) {
    case VK_FORMAT_D16_UNORM: case VK_FORMAT_X8_D24_UNORM_PACK32: case VK_FORMAT_D32_SFLOAT:
    case VK_FORMAT_D16_UNORM_S8_UINT: case VK_FORMAT_D24_UNORM_S8_UINT: case VK_FORMAT_D32_SFLOAT_S8_UINT: return VK_IMAGE_ASPECT_DEPTH_BIT;
    case VK_FORMAT_S8_UINT: return VK_IMAGE_ASPECT_STENCIL_BIT;
    default: return VK_IMAGE_ASPECT_COLOR_BIT;
    }
}
static VkImageAspectFlags nvmtl_barrier_aspect(const nvk_image *img)
{
    switch (img ? img->fmt : 0) {
    case VK_FORMAT_D16_UNORM: case VK_FORMAT_X8_D24_UNORM_PACK32: case VK_FORMAT_D32_SFLOAT: return VK_IMAGE_ASPECT_DEPTH_BIT;
    case VK_FORMAT_D16_UNORM_S8_UINT: case VK_FORMAT_D24_UNORM_S8_UINT: case VK_FORMAT_D32_SFLOAT_S8_UINT:
        return VK_IMAGE_ASPECT_DEPTH_BIT | VK_IMAGE_ASPECT_STENCIL_BIT;
    case VK_FORMAT_S8_UINT: return VK_IMAGE_ASPECT_STENCIL_BIT;
    default: return VK_IMAGE_ASPECT_COLOR_BIT;
    }
}

#include <unistd.h>
#include <limits.h>
#include <sys/stat.h>
#include <pthread.h>
static pthread_mutex_t g_gpu_clock_lock = PTHREAD_MUTEX_INITIALIZER;
static inline const char *nvmtl_pick_vk(const char *staged, const char *dev) {
    return access(staged, R_OK) == 0 ? staged : dev;
}
#define NVMTL_LOADER nvmtl_pick_vk("/Library/GPUBundles/nvmtl/libvulkan.dylib", \
                                   "/Library/GPUBundles/nvmtl/libvulkan.dylib")
#define NVMTL_ICD    nvmtl_pick_vk("/Library/GPUBundles/nvmtl/nvk_icd.json", \
                                   "/Library/GPUBundles/nvmtl/nvk_icd.json")
static inline const char *nvmtl_icd_lib_path(void) {
    const char *e = getenv("NVMTL_ICD_LIB"); if (e && access(e, R_OK) == 0) return e;
    return nvmtl_pick_vk("/Library/GPUBundles/nvmtl/libvulkan_nouveau.dylib",
                         "/Library/GPUBundles/nvmtl/libvulkan_nouveau.dylib");
}
#define NVMTL_ICD_LIB nvmtl_icd_lib_path()

static void *g_lib;
static int g_sample_loader_unwrapped;
static VkInstance g_inst; static VkPhysicalDevice g_pdev; static VkDevice g_dev; static VkQueue g_queue;
static int g_host_query_reset, g_occlusion_precise;
static int g_atomic64_buffer, g_atomic64_shared;
static int g_coop_matrix, g_float16, g_vulkan_memory_model, g_vulkan_memory_device_scope, g_storage16, g_uniform_storage16;
static int g_bindless;
static VkPhysicalDeviceMemoryProperties g_memp;
static uint32_t g_qfam = UINT32_MAX;
static uint32_t g_timestamp_bits;
static PFN_vkGetCalibratedTimestampsEXT pvkGetCalibratedTimestampsEXT;
static nvmtl_gpu_clock g_gpu_clock;
static char g_name[256];
static VkPhysicalDeviceLimits g_lim;
static uint32_t g_sm_count;
static _Atomic int g_state;
static _Atomic long g_slow_submit_ms;
static _Atomic uint64_t g_submit_wait_ns, g_submit_waits;
void nvmtl_vk_submit_wait_stats(uint64_t *total_ns, uint64_t *waits)
{
    if (total_ns) *total_ns = atomic_load(&g_submit_wait_ns);
    if (waits) *waits = atomic_load(&g_submit_waits);
}
static long nvmtl_gpu_wait_budget_ms(void)
{
    static long cached;
    if (cached) return cached;
    const char *e = getenv("NVRM_FLUSH_TIMEOUT_MS");
    long v = e ? atol(e) : 0;
    if (v > 0) { cached = v; return cached; }
    const char *me = getprogname();
    cached = (me && strcmp(me, "WindowServer") == 0) ? 5000 : 60000;
    return cached;
}
static size_t g_alloc_bytes;
static uint64_t g_buf_made, g_buf_gone, g_img_made, g_img_gone;
static uint64_t g_pool_blocks, g_pool_live, g_pool_bytes;
static uint64_t g_img_align, g_sys_align;
void (*nvmtl_purge_report_hook)(const char *why);
static uint64_t g_budget_delta;
static int g_budget_stale;
static void nvmtl_vk_budget_charge(uint64_t bytes)
{
    g_budget_delta += (bytes + 65535ull) & ~65535ull;
}
static uint64_t g_vram_bytes, g_sys_bytes, g_spilled;
static int g_budget_ext;
static int g_rt;
#define NVMTL_RT_BUFFER_USAGE (g_rt ? (VK_BUFFER_USAGE_ACCELERATION_STRUCTURE_STORAGE_BIT_KHR | VK_BUFFER_USAGE_ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_BIT_KHR) : 0)
static PFN_vkCreateAccelerationStructureKHR pvkCreateAccelerationStructureKHR;
static PFN_vkDestroyAccelerationStructureKHR pvkDestroyAccelerationStructureKHR;
static PFN_vkGetAccelerationStructureBuildSizesKHR pvkGetAccelerationStructureBuildSizesKHR;
static PFN_vkCmdBuildAccelerationStructuresKHR pvkCmdBuildAccelerationStructuresKHR;
static PFN_vkGetAccelerationStructureDeviceAddressKHR pvkGetAccelerationStructureDeviceAddressKHR;
static PFN_vkCmdCopyAccelerationStructureKHR pvkCmdCopyAccelerationStructureKHR;
#define NVMTL_VRAM_NONIMAGE (2655ull << 20)
#define NVMTL_VRAM_HEADROOM_DEFAULT (1024ull << 20)
static uint64_t nvmtl_vk_headroom(void)
{
    static long long ovr = -2;
    if (ovr == -2) { const char *e = getenv("NVMTL_VRAM_HEADROOM_MB"); ovr = (e && *e) ? atoll(e) : -1; }
    return ovr >= 0 ? (uint64_t)ovr << 20 : NVMTL_VRAM_HEADROOM_DEFAULT;
}
#define NVMTL_VRAM_HEADROOM nvmtl_vk_headroom()
static int nvmtl_vk_vram_wall(uint64_t want, uint64_t *freev_out, uint64_t *bud_out, uint64_t *used_out, int *measured_out);

static uint64_t g_buf_vram_refused;
static uint64_t g_heap_vram_refused;
static uint64_t g_cmd_begun, g_cmd_submitted, g_cmd_abandoned;
void nvmtl_vk_cmd_counts(uint64_t *begun, uint64_t *submitted, uint64_t *abandoned)
{
    if (begun) *begun = g_cmd_begun; if (submitted) *submitted = g_cmd_submitted; if (abandoned) *abandoned = g_cmd_abandoned;
}
void nvmtl_vk_counts(uint64_t *bm, uint64_t *bg, uint64_t *im, uint64_t *ig, size_t *bytes)
{
    if (bm) *bm = g_buf_made;  if (bg) *bg = g_buf_gone;
    if (im) *im = g_img_made;  if (ig) *ig = g_img_gone;
    if (bytes) *bytes = g_alloc_bytes;
}

#define INSTANCE_FNS(X) X(vkEnumeratePhysicalDevices) X(vkGetPhysicalDeviceProperties) X(vkGetPhysicalDeviceFeatures) \
    X(vkEnumerateDeviceExtensionProperties) X(vkGetPhysicalDeviceQueueFamilyProperties) X(vkGetPhysicalDeviceMemoryProperties) X(vkGetPhysicalDeviceMemoryProperties2) X(vkGetPhysicalDeviceProperties2) X(vkCreateDevice) X(vkGetDeviceProcAddr)
#define DEVICE_FNS(X) X(vkGetDeviceQueue) X(vkCreateBuffer) X(vkGetBufferMemoryRequirements) X(vkAllocateMemory) \
    X(vkBindBufferMemory) X(vkMapMemory) X(vkUnmapMemory) X(vkCreateImage) X(vkGetImageMemoryRequirements) X(vkBindImageMemory) \
    X(vkCreateImageView) X(vkCreateRenderPass) X(vkCreateFramebuffer) X(vkCreateShaderModule) X(vkDestroyShaderModule) X(vkCreatePipelineLayout) X(vkDestroyPipelineLayout) \
    X(vkDestroyPipeline) X(vkDestroyRenderPass) X(vkCreateGraphicsPipelines) X(vkCreateComputePipelines) X(vkCmdDispatch) X(vkCmdPushConstants) X(vkCreateCommandPool) X(vkAllocateCommandBuffers) X(vkBeginCommandBuffer) \
    X(vkCmdBeginRenderPass) X(vkCmdBindPipeline) X(vkCmdDraw) X(vkCmdEndRenderPass) X(vkEndCommandBuffer) \
    X(vkQueueSubmit) X(vkQueueBindSparse) X(vkCreateFence) X(vkWaitForFences) X(vkResetFences) X(vkDestroyFence) X(vkCmdCopyImageToBuffer) X(vkCmdCopyBufferToImage) X(vkCmdCopyBuffer) \
    X(vkCmdPipelineBarrier) X(vkCmdClearColorImage) X(vkCmdClearDepthStencilImage) X(vkDeviceWaitIdle) X(vkFreeCommandBuffers) X(vkCmdSetViewport) X(vkCmdSetScissor) X(vkCmdSetPrimitiveTopology) X(vkCmdSetPrimitiveRestartEnable) X(vkCmdBindVertexBuffers) X(vkCreateDescriptorSetLayout) X(vkCreateDescriptorPool) X(vkAllocateDescriptorSets) X(vkUpdateDescriptorSets) X(vkCmdBindDescriptorSets) X(vkCreateSampler) X(vkDestroySampler) X(vkFreeDescriptorSets) X(vkCmdSetBlendConstants) X(vkCmdSetStencilReference) X(vkCmdSetDepthBias) X(vkCmdFillBuffer) X(vkCmdBindIndexBuffer) X(vkCmdDrawIndexed) X(vkCmdCopyImage) X(vkCmdBlitImage) X(vkGetBufferDeviceAddress) X(vkCmdWriteTimestamp) X(vkCreateQueryPool) X(vkCmdResetQueryPool) X(vkCmdBeginQuery) X(vkCmdEndQuery) X(vkCmdCopyQueryPoolResults) X(vkGetQueryPoolResults) X(vkDestroyQueryPool) X(vkResetQueryPool) X(vkCmdDrawIndirect) X(vkCmdDrawIndexedIndirect) X(vkCmdDispatchIndirect) X(vkCmdSetCullMode) X(vkCmdSetFrontFace) X(vkCmdSetDepthTestEnable) X(vkCmdSetDepthWriteEnable) X(vkCmdSetDepthCompareOp) \
    X(vkDestroyBuffer) X(vkFreeMemory) X(vkDestroyImage) X(vkDestroyImageView) X(vkDestroyFramebuffer) X(vkDestroyCommandPool) \
    X(vkCmdSetStencilTestEnable) X(vkCmdSetStencilOp) X(vkCmdSetStencilCompareMask) X(vkCmdSetStencilWriteMask)
static int g_dyn3;
static int g_tess;
static int g_indep;
static int g_a2one;
static int g_dual;
static int g_wide;
static PFN_vkCmdSetLineWidth pvkCmdSetLineWidth;
static int g_pstat;
static PFN_vkGetPipelineExecutablePropertiesKHR p_pstat_props;
static PFN_vkGetPipelineExecutableStatisticsKHR p_pstat_stats;
static int g_mutable;
static int g_mutable_layout;
static PFN_vkCreateBufferView pvkCreateBufferView; static PFN_vkDestroyBufferView pvkDestroyBufferView;
static void *g_dummyTexAlias, *g_dummyTexView;
static void nvmtl_spirv_texel_mask(const void *p, size_t n, uint64_t out[4]);
static int g_host_import; static size_t g_host_align;
static int g_linmod;
static PFN_vkGetMemoryHostPointerPropertiesEXT pvkGetMemoryHostPointerPropertiesEXT;
static int g_sample_locations;
#include "nvmtl_sample_depth_contract.h"
static VkResult (*pnvk_sample_depth_contract)(VkDevice, uint32_t *, uint32_t *);
static VkResult (*pnvk_sample_depth_image)(VkDevice, VkImage, uint32_t *);
static uint32_t g_sample_depth_flags, g_sample_depth_counts;
static VkPhysicalDeviceSampleLocationsPropertiesEXT g_sample_props;
static PFN_vkCmdSetSampleLocationsEXT pvkCmdSetSampleLocationsEXT;
static int g_clip;
static int g_topo_any;
static PFN_vkCmdSetPolygonModeEXT pvkCmdSetPolygonModeEXT;
static PFN_vkCmdSetDepthClipEnableEXT pvkCmdSetDepthClipEnableEXT;
static PFN_vkCmdBindVertexBuffers2 pvkCmdBindVertexBuffers2;
static PFN_vkCreateRenderPass2 pvkCreateRenderPass2;
static uint32_t g_dsr_depth_modes, g_dsr_stencil_modes, g_dsr_indep, g_dsr_indep_none;
static void *nvmtl_rp_shape_variant(nvk_pipeline *p, uint32_t load, uint32_t prm, uint32_t dmode, uint32_t smode);
#define DECL(f) static PFN_##f p##f;
static PFN_vkGetInstanceProcAddr pvkGetInstanceProcAddr; static PFN_vkCreateInstance pvkCreateInstance;
INSTANCE_FNS(DECL) DEVICE_FNS(DECL)
#undef DECL
#define LOAD_I(f) do { p##f = (PFN_##f)pvkGetInstanceProcAddr(g_inst, #f); if (!p##f) { nvlog("vk: missing %s", #f); return -1; } } while (0);
#define LOAD_D(f) do { p##f = (PFN_##f)pvkGetDeviceProcAddr(g_dev, #f);   if (!p##f) { nvlog("vk: missing %s", #f); return -1; } } while (0);

_Static_assert(sizeof(VkBuffer) == sizeof(void *) && _Alignof(VkBuffer) == _Alignof(void *), "VkBuffer bridge ABI");
static VkResult nvmtl_opaque_CreateBuffer(VkDevice dev, const VkBufferCreateInfo *info, const VkAllocationCallbacks *allocator, void **out)
{
    VkBuffer handle = VK_NULL_HANDLE;
    VkResult result = pvkCreateBuffer(dev, info, allocator, &handle);
    if (result == VK_SUCCESS) *out = handle;
    return result;
}
_Static_assert(sizeof(VkDeviceMemory) == sizeof(void *) && _Alignof(VkDeviceMemory) == _Alignof(void *), "VkDeviceMemory bridge ABI");
static VkResult nvmtl_opaque_AllocateMemory(VkDevice dev, const VkMemoryAllocateInfo *info, const VkAllocationCallbacks *allocator, void **out)
{
    VkDeviceMemory handle = VK_NULL_HANDLE;
    VkResult result = pvkAllocateMemory(dev, info, allocator, &handle);
    if (result == VK_SUCCESS) *out = handle;
    return result;
}
_Static_assert(sizeof(VkImage) == sizeof(void *) && _Alignof(VkImage) == _Alignof(void *), "VkImage bridge ABI");
static VkImageCreateFlags nvmtl_sample_depth_flags(const VkImageCreateInfo *info)
{
    if (!g_sample_locations || info->tiling != VK_IMAGE_TILING_OPTIMAL ||
        !(info->usage & VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT)) return info->flags;
    switch (info->format) {
    case VK_FORMAT_D16_UNORM: case VK_FORMAT_X8_D24_UNORM_PACK32: case VK_FORMAT_D32_SFLOAT:
    case VK_FORMAT_D16_UNORM_S8_UINT: case VK_FORMAT_D24_UNORM_S8_UINT: case VK_FORMAT_D32_SFLOAT_S8_UINT:
        return info->flags | VK_IMAGE_CREATE_SAMPLE_LOCATIONS_COMPATIBLE_DEPTH_BIT_EXT;
    default: return info->flags;
    }
}
static VkResult nvmtl_typed_CreateImage(VkDevice dev, const VkImageCreateInfo *info, const VkAllocationCallbacks *allocator, VkImage *out)
{
    if (!info || !out) return VK_ERROR_UNKNOWN;
    VkImageCreateInfo adjusted = *info;
    adjusted.flags = nvmtl_sample_depth_flags(info);
    VkImage handle = VK_NULL_HANDLE;
    VkResult result = pvkCreateImage(dev, &adjusted, allocator, &handle);
    if (result != VK_SUCCESS) return result;
    if (g_sample_locations &&
        (adjusted.flags & VK_IMAGE_CREATE_SAMPLE_LOCATIONS_COMPATIBLE_DEPTH_BIT_EXT)) {
        uint32_t flags = 0;
        result = pnvk_sample_depth_image ? pnvk_sample_depth_image(dev, handle, &flags) : VK_ERROR_FEATURE_NOT_PRESENT;
        if (result != VK_SUCCESS || (flags & NVMTL_SAMPLE_DEPTH_CONTRACT_REQUIRED) != NVMTL_SAMPLE_DEPTH_CONTRACT_REQUIRED) {
            pvkDestroyImage(dev, handle, allocator);
            return result != VK_SUCCESS ? result : VK_ERROR_FEATURE_NOT_PRESENT;
        }
    }
    *out = handle;
    return VK_SUCCESS;
}
static VkResult nvmtl_opaque_CreateImage(VkDevice dev, const VkImageCreateInfo *info, const VkAllocationCallbacks *allocator, void **out)
{
    VkImage handle = VK_NULL_HANDLE;
    VkResult result = nvmtl_typed_CreateImage(dev, info, allocator, &handle);
    if (result == VK_SUCCESS) *out = handle;
    return result;
}
_Static_assert(sizeof(VkImageView) == sizeof(void *) && _Alignof(VkImageView) == _Alignof(void *), "VkImageView bridge ABI");
static VkResult nvmtl_opaque_CreateImageView(VkDevice dev, const VkImageViewCreateInfo *info, const VkAllocationCallbacks *allocator, void **out)
{
    VkImageView handle = VK_NULL_HANDLE;
    VkResult result = pvkCreateImageView(dev, info, allocator, &handle);
    if (result == VK_SUCCESS) *out = handle;
    return result;
}
_Static_assert(sizeof(VkPipelineLayout) == sizeof(void *) && _Alignof(VkPipelineLayout) == _Alignof(void *), "VkPipelineLayout bridge ABI");
static VkResult nvmtl_opaque_CreatePipelineLayout(VkDevice dev, const VkPipelineLayoutCreateInfo *info, const VkAllocationCallbacks *allocator, void **out)
{
    VkPipelineLayout handle = VK_NULL_HANDLE;
    VkResult result = pvkCreatePipelineLayout(dev, info, allocator, &handle);
    if (result == VK_SUCCESS) *out = handle;
    return result;
}
_Static_assert(sizeof(VkCommandPool) == sizeof(void *) && _Alignof(VkCommandPool) == _Alignof(void *), "VkCommandPool bridge ABI");
static VkResult nvmtl_opaque_CreateCommandPool(VkDevice dev, const VkCommandPoolCreateInfo *info, const VkAllocationCallbacks *allocator, void **out)
{
    VkCommandPool handle = VK_NULL_HANDLE;
    VkResult result = pvkCreateCommandPool(dev, info, allocator, &handle);
    if (result == VK_SUCCESS) *out = handle;
    return result;
}
static VkResult nvmtl_opaque_CreateGraphicsPipelines(VkDevice dev, VkPipelineCache cache, uint32_t count, const VkGraphicsPipelineCreateInfo *info, const VkAllocationCallbacks *allocator, void **out)
{
    if (count != 1) return VK_ERROR_INITIALIZATION_FAILED;
    VkPipeline handle = VK_NULL_HANDLE;
    VkGraphicsPipelineCreateInfo pinfo;
    if (g_pstat) { pinfo = *info; pinfo.flags |= VK_PIPELINE_CREATE_CAPTURE_STATISTICS_BIT_KHR; info = &pinfo; }
    VkResult result = pvkCreateGraphicsPipelines(dev, cache, count, info, allocator, &handle);
    if (result == VK_SUCCESS) *out = handle;
    else if (handle) pvkDestroyPipeline(dev, handle, allocator);
    return result;
}
int nvmtl_vk_pipeline_stats(void *pipe, char *out, size_t n)
{
    if (!n) return -1; out[0] = 0;
    if (!g_pstat || !pipe || !p_pstat_props || !p_pstat_stats) return -1;
    VkPipelineInfoKHR pi = { VK_STRUCTURE_TYPE_PIPELINE_INFO_KHR, NULL, (VkPipeline)pipe };
    uint32_t ne = 0; if (p_pstat_props(g_dev, &pi, &ne, NULL) != VK_SUCCESS || !ne) return -1;
    VkPipelineExecutablePropertiesKHR ep[4]; if (ne > 4) ne = 4;
    for (uint32_t i = 0; i < ne; i++) ep[i] = (VkPipelineExecutablePropertiesKHR){ VK_STRUCTURE_TYPE_PIPELINE_EXECUTABLE_PROPERTIES_KHR };
    if (p_pstat_props(g_dev, &pi, &ne, ep) < 0) return -1;
    static const char *lname[] = { "Instruction count", "Static cycle count", "Max warps/SM", "Number of GPRs", "Spills to memory",
                                   "Fills from memory", "Spills to reg", "Fills from reg", "Code size", "SLM size" };
    static const char *sname[] = { "instr", "cyc", "warps", "gpr", "spillM", "fillM", "spillR", "fillR", "code", "slm" };
    size_t w = 0;
    for (uint32_t e = 0; e < ne && w + 1 < n; e++) {
        int k = snprintf(out + w, n - w, "%s%s:", e ? " | " : "", ep[e].name); if (k > 0) w += (size_t)k < n - w ? (size_t)k : n - w - 1;
        VkPipelineExecutableInfoKHR ei = { VK_STRUCTURE_TYPE_PIPELINE_EXECUTABLE_INFO_KHR, NULL, (VkPipeline)pipe, e };
        VkPipelineExecutableStatisticKHR st[16]; uint32_t ns = 16;
        for (uint32_t i = 0; i < ns; i++) st[i] = (VkPipelineExecutableStatisticKHR){ VK_STRUCTURE_TYPE_PIPELINE_EXECUTABLE_STATISTIC_KHR };
        if (p_pstat_stats(g_dev, &ei, &ns, st) < 0) continue;
        for (uint32_t i = 0; i < ns && w + 1 < n; i++) {
            const char *nm = st[i].name; for (unsigned j = 0; j < sizeof lname / sizeof *lname; j++) if (!strcmp(nm, lname[j])) { nm = sname[j]; break; }
            unsigned long long v = st[i].format == VK_PIPELINE_EXECUTABLE_STATISTIC_FORMAT_INT64_KHR ? (unsigned long long)st[i].value.i64
                                 : st[i].format == VK_PIPELINE_EXECUTABLE_STATISTIC_FORMAT_FLOAT64_KHR ? (unsigned long long)st[i].value.f64
                                 : st[i].format == VK_PIPELINE_EXECUTABLE_STATISTIC_FORMAT_BOOL32_KHR ? (unsigned long long)st[i].value.b32 : st[i].value.u64;
            k = snprintf(out + w, n - w, " %s %llu", nm, v); if (k > 0) w += (size_t)k < n - w ? (size_t)k : n - w - 1;
        }
    }
    return 0;
}
static VkResult nvmtl_opaque_CreateComputePipelines(VkDevice dev, VkPipelineCache cache, uint32_t count, const VkComputePipelineCreateInfo *info, const VkAllocationCallbacks *allocator, void **out)
{
    if (count != 1) return VK_ERROR_INITIALIZATION_FAILED;
    VkPipeline handle = VK_NULL_HANDLE;
    VkResult result = pvkCreateComputePipelines(dev, cache, count, info, allocator, &handle);
    if (result == VK_SUCCESS) *out = handle;
    else if (handle) pvkDestroyPipeline(dev, handle, allocator);
    return result;
}
_Static_assert(sizeof(VkPipeline) == sizeof(void *) && _Alignof(VkPipeline) == _Alignof(void *), "VkPipeline bridge ABI");
_Static_assert(sizeof(VkCommandBuffer) == sizeof(void *) && _Alignof(VkCommandBuffer) == _Alignof(void *), "VkCommandBuffer bridge ABI");
static VkResult nvmtl_opaque_AllocateCommandBuffers(VkDevice dev, const VkCommandBufferAllocateInfo *info, void **out)
{
    if (info->commandBufferCount != 1) return VK_ERROR_INITIALIZATION_FAILED;
    VkCommandBuffer handle = VK_NULL_HANDLE;
    VkResult result = pvkAllocateCommandBuffers(dev, info, &handle);
    if (result == VK_SUCCESS) *out = handle;
    return result;
}

#define VKCK(x, what) do { VkResult r_ = (x); if (r_ != VK_SUCCESS) { nvlog("vk: %s -> %d", what, r_); return -1; } } while (0)
#include <fcntl.h>
typedef struct { uint32_t version, surfaceID, plane, pad; uint64_t size_B; } nvmtl_vram_rec;
static void (*pnvk_stage_import)(const nvmtl_vram_rec *);
static uint32_t (*pnvk_surface_dirty)(VkDevice, uint32_t, uint32_t);
static int (*pnvk_sparse)(void);
static int g_qsparse, g_sparse;
static int g_extfd;

int nvmtl_vk_format_block(uint32_t f, uint32_t *bw, uint32_t *bh, uint32_t *block_bytes)
{
    if (f >= 131 && f <= 146) { *bw = *bh = 4; *block_bytes = (f <= 134 || f == 139 || f == 140) ? 8 : 16; return 1; }
    *bw = *bh = 1; *block_bytes = 0; return 0;
}
static inline uint32_t nvmtl_blocks(uint32_t v, uint32_t b) { return b > 1 ? (v + b - 1) / b : v; }

static int nvmtl_spirv_bands_ok(const void *p, size_t n, const char *what);
static int nvmtl_spirv_ok(const void *p, size_t n, const char *what)
{
    if (!p || n < 20 || (n & 3u)) {
        nvlog("vk: %s is not SPIR-V (%zu bytes) — refusing to build a pipeline from it", what, n);
        return 0;
    }
    uint32_t magic = ((const uint32_t *)p)[0];
    if (magic != 0x07230203u) {
        nvlog("vk: %s is not SPIR-V (first word 0x%08x, %zu bytes) — refusing to build a pipeline from it",
              what, magic, n);
        return 0;
    }
    if (!(g_atomic64_buffer && g_atomic64_shared)) {
        const uint32_t *w = p;
        size_t words = n / sizeof *w;
        for (size_t i = 5; i < words; ) {
            uint32_t count = w[i] >> 16, op = w[i] & 0xffffu;
            if (!count || count > words - i) return 0;
            if (op == 17 && count == 2 && w[i + 1] == 12) {
                nvlog("vk: %s requires integer atomic64 features unavailable on this device", what);
                return 0;
            }
            i += count;
        }
    }
    {
        const uint32_t *w = p; size_t words = n / sizeof *w;
        for (size_t i = 5; i < words; ) {
            uint32_t count = w[i] >> 16, op = w[i] & 0xffffu;
            if (!count || count > words - i) return 0;
            if (op == 17 && count == 2) {
                uint32_t cap = w[i + 1];
                int supported = cap == 9 ? g_float16 : cap == 4433 ? g_storage16 : cap == 4434 ? g_uniform_storage16 :
                                cap == 5345 ? g_vulkan_memory_model : cap == 5346 ? g_vulkan_memory_device_scope :
                                cap == 6022 ? g_coop_matrix : 1;
                if (!supported) { nvlog("vk: %s requires unavailable enabled capability %u", what, cap); return 0; }
            }
            i += count;
        }
    }
    return nvmtl_spirv_bands_ok(p, n, what);
}

#define NVMTL_VKMSG_SITES    128
#define NVMTL_VKMSG_VERBATIM 8
#define NVMTL_VKMSG_EVERY    4096
static struct { uint64_t key, n; } g_vkmsg_sites[NVMTL_VKMSG_SITES];
static uint64_t g_vkmsg_over;
static int g_vkmsg_want;
static VkDebugUtilsMessengerEXT g_vkmsg;
static pthread_mutex_t g_vkmsg_lock = PTHREAD_MUTEX_INITIALIZER;
static uint64_t nvmtl_vkmsg_key(const char *id, const char *m)
{
    uint64_t h = 1469598103934665603ull; int dig = 0;
    for (const char *s = id; *s; s++) { h ^= (unsigned char)*s; h *= 1099511628211ull; }
    h ^= 0xff; h *= 1099511628211ull;
    for (const char *s = m; *s; s++) {
        int d = *s >= '0' && *s <= '9'; if (d && dig) continue; dig = d;
        h ^= d ? (unsigned char)'#' : (unsigned char)*s; h *= 1099511628211ull;
    }
    return h ? h : 1;
}
static int nvmtl_vkmsg_budget(uint64_t key, uint64_t *rep, int *full)
{
    *rep = 0; *full = 0; uint64_t n; int i = 0;
    pthread_mutex_lock(&g_vkmsg_lock);
    for (; i < NVMTL_VKMSG_SITES; i++) {
        if (g_vkmsg_sites[i].key == key) break;
        if (!g_vkmsg_sites[i].key) { g_vkmsg_sites[i].key = key; break; }
    }
    if (i < NVMTL_VKMSG_SITES) n = ++g_vkmsg_sites[i].n; else { n = ++g_vkmsg_over; *full = 1; }
    pthread_mutex_unlock(&g_vkmsg_lock);
    if (n <= NVMTL_VKMSG_VERBATIM) return 1;
    if (n % NVMTL_VKMSG_EVERY == 0) { *rep = n; return 1; }
    return 0;
}
static VkBool32 VKAPI_PTR nvmtl_vkmsg_cb(VkDebugUtilsMessageSeverityFlagBitsEXT sev, VkDebugUtilsMessageTypeFlagsEXT type,
                                         const VkDebugUtilsMessengerCallbackDataEXT *d, void *user)
{
    (void)type; (void)user;
    const char *id = d && d->pMessageIdName ? d->pMessageIdName : "?", *m = d && d->pMessage ? d->pMessage : "(no message)";
    uint64_t rep; int full;
    if (!nvmtl_vkmsg_budget(nvmtl_vkmsg_key(id, m), &rep, &full)) return VK_FALSE;
    const char *b = strrchr(id, '/'); b = b ? b + 1 : id;
    char t[600]; snprintf(t, sizeof t, "nvk-reason: %s %s: %s", (sev & VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT) ? "E" : "W", b, m);
    for (char *c = t; *c; c++) if ((unsigned char)*c < 0x20) *c = ' ';
    extern const char *nvmtl_log_path(void);
    FILE *f = fopen(nvmtl_log_path(), "a");
    if (f) {
        if (rep) fprintf(f, "pid %d NVMTL: %s   [logged %llu times now%s]\n", getpid(), t, (unsigned long long)rep, full ? ", table full" : "");
        else     fprintf(f, "pid %d NVMTL: %s%s\n", getpid(), t, full ? "   [table full]" : "");
        fclose(f);
    }
    return VK_FALSE;
}
static int nvmtl_vkmsg_off_by_env(void) { const char *e = getenv("NVMTL_NVK_MESSAGES"); return e && !strcmp(e, "0"); }
static int nvmtl_vkmsg_listed(void)
{
    if (nvmtl_vkmsg_off_by_env()) return 0;
    PFN_vkEnumerateInstanceExtensionProperties en =
        (PFN_vkEnumerateInstanceExtensionProperties)pvkGetInstanceProcAddr(NULL, "vkEnumerateInstanceExtensionProperties");
    uint32_t n = 0; if (!en || en(NULL, &n, NULL) != VK_SUCCESS || !n) return 0;
    VkExtensionProperties *p = calloc(n, sizeof *p); if (!p) return 0;
    int hit = 0; VkResult r = en(NULL, &n, p);
    if (r == VK_SUCCESS || r == VK_INCOMPLETE)
        for (uint32_t i = 0; i < n; i++) if (!strcmp(p[i].extensionName, VK_EXT_DEBUG_UTILS_EXTENSION_NAME)) hit = 1;
    free(p); return hit;
}

static int nvmtl_sample_loader_diagnostic(void)
{
    if (pnvk_sample_depth_contract || pnvk_sample_depth_image) return 0;
    const char *layers = getenv("VK_INSTANCE_LAYERS");
    if (!layers || strcmp(layers, "VK_LAYER_KHRONOS_validation") ||
        getenv("VK_LOADER_LAYERS_ENABLE") || getenv("VK_LOADER_LAYERS_ALLOW") ||
        getenv("VK_LOADER_LAYERS_DISABLE")) return 0;
    PFN_vkEnumerateInstanceLayerProperties enlayer =
        (PFN_vkEnumerateInstanceLayerProperties)pvkGetInstanceProcAddr(NULL, "vkEnumerateInstanceLayerProperties");
    PFN_vkEnumerateInstanceExtensionProperties enext =
        (PFN_vkEnumerateInstanceExtensionProperties)pvkGetInstanceProcAddr(NULL, "vkEnumerateInstanceExtensionProperties");
    uint32_t n = 0;
    if (!enlayer || !enext || enlayer(&n, NULL) != VK_SUCCESS || n != 1) return 0;
    VkLayerProperties layer;
    if (enlayer(&n, &layer) != VK_SUCCESS || n != 1 ||
        strcmp(layer.layerName, "VK_LAYER_KHRONOS_validation")) return 0;
    n = 0;
    if (enext(layer.layerName, &n, NULL) != VK_SUCCESS || !n || n > 128) return 0;
    VkExtensionProperties *ext = calloc(n, sizeof *ext);
    if (!ext) return 0;
    int settings = 0, validation = 0;
    VkResult r = enext(layer.layerName, &n, ext);
    if (r == VK_SUCCESS) for (uint32_t i = 0; i < n; ++i) {
        settings |= !strcmp(ext[i].extensionName, VK_EXT_LAYER_SETTINGS_EXTENSION_NAME);
        validation |= !strcmp(ext[i].extensionName, VK_EXT_VALIDATION_FEATURES_EXTENSION_NAME);
    }
    free(ext); return settings && validation;
}
static VkResult nvmtl_vkmsg_create_instance(VkInstanceCreateInfo *ici)
{
    VkInstanceCreateInfo original = *ici;
    const char *ext[16]; uint32_t count = original.enabledExtensionCount;
    if (count > 13) return pvkCreateInstance(ici, NULL, &g_inst);
    for (uint32_t i = 0; i < count; ++i) ext[i] = original.ppEnabledExtensionNames[i];
    g_vkmsg_want = nvmtl_vkmsg_listed();
    const uint32_t diagnostic_count = count;
    if (g_vkmsg_want) ext[count++] = VK_EXT_DEBUG_UTILS_EXTENSION_NAME;
    g_sample_loader_unwrapped = !original.enabledLayerCount && nvmtl_sample_loader_diagnostic();
    VkBool32 wrap = VK_FALSE;
    VkLayerSettingEXT setting = { "VK_LAYER_KHRONOS_validation", "unique_handles",
        VK_LAYER_SETTING_TYPE_BOOL32_EXT, 1, &wrap };
    VkValidationFeatureEnableEXT sync = VK_VALIDATION_FEATURE_ENABLE_SYNCHRONIZATION_VALIDATION_EXT;
    VkValidationFeaturesEXT validation = { VK_STRUCTURE_TYPE_VALIDATION_FEATURES_EXT,
        original.pNext, 1, &sync, 0, NULL };
    VkLayerSettingsCreateInfoEXT settings = { VK_STRUCTURE_TYPE_LAYER_SETTINGS_CREATE_INFO_EXT,
        &validation, 1, &setting };
    const char *layer = "VK_LAYER_KHRONOS_validation";
    if (g_sample_loader_unwrapped) {
        ext[count++] = VK_EXT_LAYER_SETTINGS_EXTENSION_NAME;
        ext[count++] = VK_EXT_VALIDATION_FEATURES_EXTENSION_NAME;
        ici->pNext = &settings; ici->enabledLayerCount = 1; ici->ppEnabledLayerNames = &layer;
    }
    ici->enabledExtensionCount = count; ici->ppEnabledExtensionNames = count ? ext : NULL;
    VkResult r = pvkCreateInstance(ici, NULL, &g_inst);
    if (r != VK_SUCCESS && g_vkmsg_want) {

        for (uint32_t i = diagnostic_count; i + 1 < count; ++i) ext[i] = ext[i + 1];
        --count; g_vkmsg_want = 0;
        ici->enabledExtensionCount = count; ici->ppEnabledExtensionNames = count ? ext : NULL;
        r = pvkCreateInstance(ici, NULL, &g_inst);
    }
    if (r != VK_SUCCESS) g_sample_loader_unwrapped = 0;
    *ici = original;
    return r;
}
static void nvmtl_vkmsg_arm(void)
{
    if (!g_vkmsg_want) {
        nvlog("vk: NVK messages OFF: %s", nvmtl_vkmsg_off_by_env() ? "NVMTL_NVK_MESSAGES=0" : "the instance has no VK_EXT_debug_utils");
        return;
    }
    PFN_vkCreateDebugUtilsMessengerEXT c = (PFN_vkCreateDebugUtilsMessengerEXT)pvkGetInstanceProcAddr(g_inst, "vkCreateDebugUtilsMessengerEXT");
    VkDebugUtilsMessengerCreateInfoEXT mi = { VK_STRUCTURE_TYPE_DEBUG_UTILS_MESSENGER_CREATE_INFO_EXT, NULL, 0,
        VK_DEBUG_UTILS_MESSAGE_SEVERITY_WARNING_BIT_EXT | VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT,
        VK_DEBUG_UTILS_MESSAGE_TYPE_GENERAL_BIT_EXT | VK_DEBUG_UTILS_MESSAGE_TYPE_VALIDATION_BIT_EXT | VK_DEBUG_UTILS_MESSAGE_TYPE_PERFORMANCE_BIT_EXT,
        nvmtl_vkmsg_cb, NULL };
    VkResult r = c ? c(g_inst, &mi, NULL, &g_vkmsg) : VK_ERROR_EXTENSION_NOT_PRESENT;
    if (r != VK_SUCCESS) { g_vkmsg = VK_NULL_HANDLE; nvlog("vk: NVK messages OFF: vkCreateDebugUtilsMessengerEXT %s %d", c ? "returned" : "is missing,", r); return; }
    nvlog("vk: NVK messages ON: its own warnings and errors land here as \"nvk-reason:\"");
    const char *e = getenv("NVMTL_NVK_MESSAGES");
    if (e && !strcmp(e, "probe")) {
        PFN_vkSubmitDebugUtilsMessageEXT s = (PFN_vkSubmitDebugUtilsMessageEXT)pvkGetInstanceProcAddr(g_inst, "vkSubmitDebugUtilsMessageEXT");
        VkDebugUtilsMessengerCallbackDataEXT cd = { VK_STRUCTURE_TYPE_DEBUG_UTILS_MESSENGER_CALLBACK_DATA_EXT, NULL, 0,
            "apps-vkmsg/probe:1", 0, "probe delivered by the ICD", 0, NULL, 0, NULL, 0, NULL };
        if (s) s(g_inst, VK_DEBUG_UTILS_MESSAGE_SEVERITY_WARNING_BIT_EXT, VK_DEBUG_UTILS_MESSAGE_TYPE_GENERAL_BIT_EXT, &cd);
        else nvlog("vk: NVK messages probe: vkSubmitDebugUtilsMessageEXT is missing");
    }
}

static int nvmtl_vk_initialize(void)
{
    if (g_state) return g_state == 1 ? 0 : -1;
    g_state = -1;
    if (!getenv("NVRM_FLUSH_TIMEOUT_MS")) {
        char budget[32]; snprintf(budget, sizeof budget, "%ld", nvmtl_gpu_wait_budget_ms());
        setenv("NVRM_FLUSH_TIMEOUT_MS", budget, 1);
        nvlog("vk: GPU wait budget %s ms for %s (WindowServer keeps 5000 - a frozen desktop is worse than a dropped frame)", budget, getprogname());
    }
    if (!getenv("MESA_SHADER_CACHE_DIR") && !getenv("XDG_CACHE_HOME") && !getenv("NVMTL_NO_MESA_CACHE_DIR")) {
        const char *home = getenv("HOME");
        if (!home || access(home, W_OK) != 0) {
            char ucd[PATH_MAX], dir[PATH_MAX];
            const size_t n = confstr(_CS_DARWIN_USER_CACHE_DIR, ucd, sizeof ucd);
            if (n > 0 && n <= sizeof ucd && snprintf(dir, sizeof dir, "%snvmtl-mesa", ucd) < (int)sizeof dir) {
                mkdir(dir, 0700);
                if (access(dir, W_OK) == 0) { setenv("MESA_SHADER_CACHE_DIR", dir, 1);
                    nvlog("vk: %s has no writable HOME (%s) - Mesa shader disk cache at %s", getprogname(), home ?: "unset", dir); }
                else nvlog("vk: %s: no writable Mesa cache dir (%s) - NAK recompiles every start", getprogname(), dir);
            }
        }
    }
    g_lib = dlopen(NVMTL_ICD_LIB, RTLD_NOW | RTLD_LOCAL);
    if (g_lib) {
        pvkGetInstanceProcAddr = (PFN_vkGetInstanceProcAddr)dlsym(g_lib, "vkGetInstanceProcAddr");
        if (!pvkGetInstanceProcAddr)
            pvkGetInstanceProcAddr = (PFN_vkGetInstanceProcAddr)dlsym(g_lib, "vk_icdGetInstanceProcAddr");
        if (pvkGetInstanceProcAddr) {
            char vid[1200];
            nvlog("vk: driver opened directly: %s (image %s)", NVMTL_ICD_LIB, nvmtl_image_ident((const void *)pvkGetInstanceProcAddr, vid, sizeof vid));
            pnvk_stage_import = (void (*)(const nvmtl_vram_rec *))dlsym(g_lib, "nvmtl_nvk_stage_import");
            pnvk_surface_dirty = (uint32_t (*)(VkDevice, uint32_t, uint32_t))dlsym(g_lib, "nvmtl_nvk_surface_dirty");
            pnvk_sample_depth_contract = (VkResult (*)(VkDevice, uint32_t *, uint32_t *))dlsym(g_lib, "nvmtl_nvk_sample_depth_contract_v1");
            pnvk_sample_depth_image = (VkResult (*)(VkDevice, VkImage, uint32_t *))dlsym(g_lib, "nvmtl_nvk_sample_depth_image_v1");
            pnvk_sparse = (int (*)(void))dlsym(g_lib, "nvmtl_nvk_sparse_bind_v1");
        }
        else { nvlog("vk: %s has no vkGetInstanceProcAddr", NVMTL_ICD_LIB); dlclose(g_lib); g_lib = NULL; }
    } else {
        nvlog("vk: dlopen(%s): %s", NVMTL_ICD_LIB, dlerror());
    }
    if (!pvkGetInstanceProcAddr) {
        if (!getenv("VK_ICD_FILENAMES")) setenv("VK_ICD_FILENAMES", NVMTL_ICD, 1);
        g_lib = dlopen(NVMTL_LOADER, RTLD_NOW | RTLD_LOCAL);
        if (!g_lib) { nvlog("vk: dlopen(%s): %s", NVMTL_LOADER, dlerror()); return -1; }
        pvkGetInstanceProcAddr = (PFN_vkGetInstanceProcAddr)dlsym(g_lib, "vkGetInstanceProcAddr");
        if (!pvkGetInstanceProcAddr) { nvlog("vk: no vkGetInstanceProcAddr"); return -1; }
        nvlog("vk: fell back to the loader at %s", NVMTL_LOADER);
    }
    pvkCreateInstance = (PFN_vkCreateInstance)pvkGetInstanceProcAddr(NULL, "vkCreateInstance");
    if (!pvkCreateInstance) { nvlog("vk: no vkCreateInstance"); return -1; }

    VkApplicationInfo ai = { VK_STRUCTURE_TYPE_APPLICATION_INFO, NULL, "NVMTLDriver", 1, "NVMTL", 1, VK_API_VERSION_1_3 };
    VkInstanceCreateInfo ici = { VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, NULL, 0, &ai, 0, NULL, 0, NULL };
    VKCK(nvmtl_vkmsg_create_instance(&ici), "vkCreateInstance");
    nvmtl_vkmsg_arm();
    INSTANCE_FNS(LOAD_I)

    uint32_t n = 0; VkResult er = pvkEnumeratePhysicalDevices(g_inst, &n, NULL);
    if (er != VK_SUCCESS || !n) {
        nvlog("vk: zero physical devices through this ICD — vkEnumeratePhysicalDevices -> VkResult %d, count %u, in %s (uid %u euid %u): %s",
              (int)er, n, getprogname(), (unsigned)getuid(), (unsigned)geteuid(),
              er != VK_SUCCESS ? "the ENUMERATION failed, so NVK could not reach the device (not an absent GPU)"
                               : "the enumeration SUCCEEDED and found no device through this ICD");
        return -1; }
    nvlog("vk: pdev %u physical device(s) through this ICD (VkResult %d)", n, (int)er);
    VkPhysicalDevice pds[8]; if (n > 8) n = 8; pvkEnumeratePhysicalDevices(g_inst, &n, pds); g_pdev = pds[0];
    VkPhysicalDeviceProperties pp; pvkGetPhysicalDeviceProperties(g_pdev, &pp);
    snprintf(g_name, sizeof g_name, "%s", pp.deviceName);
    g_lim = pp.limits;
    { VkPhysicalDeviceShaderSMBuiltinsPropertiesNV sm = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SHADER_SM_BUILTINS_PROPERTIES_NV };
      VkPhysicalDeviceProperties2 p2 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, &sm };
      pvkGetPhysicalDeviceProperties2(g_pdev, &p2); g_sm_count = sm.shaderSMCount;
      nvlog("vk: limits — image array layers %u, shared memory %u B, workgroup %ux%ux%u (%u invocations), SMs %u",
            g_lim.maxImageArrayLayers, g_lim.maxComputeSharedMemorySize, g_lim.maxComputeWorkGroupSize[0], g_lim.maxComputeWorkGroupSize[1],
            g_lim.maxComputeWorkGroupSize[2], g_lim.maxComputeWorkGroupInvocations, g_sm_count); }
    pvkGetPhysicalDeviceMemoryProperties(g_pdev, &g_memp);

    uint32_t qn = 0; pvkGetPhysicalDeviceQueueFamilyProperties(g_pdev, &qn, NULL);
    VkQueueFamilyProperties qf[16]; if (qn > 16) qn = 16; pvkGetPhysicalDeviceQueueFamilyProperties(g_pdev, &qn, qf);
    for (uint32_t i = 0; i < qn; i++)
        if ((qf[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) && (qf[i].queueFlags & VK_QUEUE_COMPUTE_BIT)) { g_qfam = i; g_qsparse = (qf[i].queueFlags & VK_QUEUE_SPARSE_BINDING_BIT) != 0; break; }
    if (g_qfam == UINT32_MAX) { nvlog("vk: no graphics+compute queue family"); return -1; }
    g_timestamp_bits = qf[g_qfam].timestampValidBits;

    float prio = 1.0f;
    VkDeviceQueueCreateInfo qci = { VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, NULL, 0, g_qfam, 1, &prio };

    static const char *kWant[14] = { "VK_EXT_extended_dynamic_state3", "VK_EXT_depth_clip_enable", "VK_EXT_memory_budget",
                                    "VK_KHR_acceleration_structure", "VK_KHR_ray_query", "VK_KHR_deferred_host_operations",
                                    "VK_EXT_external_memory_host",
                                    "VK_EXT_image_drm_format_modifier",
                                    "VK_KHR_external_memory_fd",
                                    "VK_EXT_mutable_descriptor_type", "VK_KHR_cooperative_matrix", "VK_EXT_calibrated_timestamps", "VK_EXT_sample_locations", "VK_KHR_pipeline_executable_properties"  };
    const char *exts[14]; uint32_t next = 0; int have[14] = {0};
    uint32_t na = 0;
    if (pvkEnumerateDeviceExtensionProperties &&
        pvkEnumerateDeviceExtensionProperties(g_pdev, NULL, &na, NULL) == VK_SUCCESS && na) {
        VkExtensionProperties *props = calloc(na, sizeof *props);
        if (props && pvkEnumerateDeviceExtensionProperties(g_pdev, NULL, &na, props) == VK_SUCCESS)
            for (uint32_t i = 0; i < na; i++)
                for (int w = 0; w < 14; w++)
                    if (!have[w] && !strcmp(props[i].extensionName, kWant[w])) { have[w] = 1; exts[next++] = kWant[w]; }
        free(props);
    }
    if (have[12]) {
        PFN_vkGetPhysicalDeviceProperties2 props2 = (PFN_vkGetPhysicalDeviceProperties2)pvkGetInstanceProcAddr(g_inst, "vkGetPhysicalDeviceProperties2");
        g_sample_props = (VkPhysicalDeviceSampleLocationsPropertiesEXT){ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SAMPLE_LOCATIONS_PROPERTIES_EXT };
        VkPhysicalDeviceProperties2 pp = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, &g_sample_props };
        if (props2) props2(g_pdev, &pp);
        g_sample_locations = props2 && g_lim.maxPushConstantsSize >= NVMTL_SAMPLE_POSITION_OFFSET + NVMTL_SAMPLE_POSITION_BYTES && g_sample_props.variableSampleLocations &&
            g_sample_props.maxSampleLocationGridSize.width >= 1 && g_sample_props.maxSampleLocationGridSize.height >= 1 &&
            (g_sample_props.sampleLocationSampleCounts & 15u) == 15u && g_sample_props.sampleLocationSubPixelBits == 4 &&
            g_sample_props.sampleLocationCoordinateRange[0] <= 0 && g_sample_props.sampleLocationCoordinateRange[1] >= 0.9375f;
    }
    g_dyn3 = have[0]; g_clip = have[0] && have[1]; g_budget_ext = have[2]; g_rt = have[3] && have[4] && have[5];
    g_host_import = have[6];
    g_linmod = have[7];
    g_extfd = have[8];
    { const char *e = getenv("NVMTL_PSTAT"); g_pstat = have[13] && e && *e == '1'; }
    g_mutable = have[9] && !getenv("NVMTL_NO_TEXBUF");
    nvlog("vk: par3 VK_EXT_mutable_descriptor_type %s — texture buffers %s", have[9] ? "present" : "ABSENT",
          g_mutable ? "ON (mutable texture/storage bands)" : have[9] ? "OFF (NVMTL_NO_TEXBUF)" : "OFF");
    nvlog("vk: VK_EXT_image_drm_format_modifier %s - a buffer texture's row pitch is %s", have[7] ? "present" : "ABSENT",
          have[7] ? "the app's own bytesPerRow (explicit DRM linear layout)" : "what LINEAR tiling picks; any other bytesPerRow is refused");
    nvlog("vk: VK_EXT_memory_budget %s — VRAM budget is %s", have[2] ? "present" : "ABSENT", have[2] ? "MEASURED" : "estimated at 70 %% of the card");
    nvlog("vk: %s %s · %s %s", kWant[0], have[0] ? "present" : "ABSENT", kWant[1], have[1] ? "present" : "ABSENT");
    VkPhysicalDeviceRayQueryFeaturesKHR rqf = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_RAY_QUERY_FEATURES_KHR, NULL, VK_TRUE };
    VkPhysicalDeviceAccelerationStructureFeaturesKHR asf = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ACCELERATION_STRUCTURE_FEATURES_KHR, &rqf,
                                                            VK_TRUE, VK_FALSE, VK_FALSE, VK_FALSE, VK_TRUE };
    VkPhysicalDeviceVulkan12Features v12 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES, g_rt ? (void *)&asf : NULL };
    v12.bufferDeviceAddress = VK_TRUE;
    VkPhysicalDeviceVulkan11Features v11 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_FEATURES, &v12 };
    VkPhysicalDeviceCooperativeMatrixFeaturesKHR cmf = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_COOPERATIVE_MATRIX_FEATURES_KHR };
    static VkPhysicalDeviceMutableDescriptorTypeFeaturesEXT mdf;
    if (g_mutable) { mdf = (VkPhysicalDeviceMutableDescriptorTypeFeaturesEXT){ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MUTABLE_DESCRIPTOR_TYPE_FEATURES_EXT, v12.pNext, VK_TRUE }; v12.pNext = &mdf; }
    static VkPhysicalDevicePipelineExecutablePropertiesFeaturesKHR pef;
    if (g_pstat) { pef = (VkPhysicalDevicePipelineExecutablePropertiesFeaturesKHR){ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PIPELINE_EXECUTABLE_PROPERTIES_FEATURES_KHR, v12.pNext, VK_TRUE }; v12.pNext = &pef; }
    VkPhysicalDeviceDepthClipEnableFeaturesEXT clipf = {
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DEPTH_CLIP_ENABLE_FEATURES_EXT, NULL, VK_TRUE };
    if (g_clip) clipf.pNext = &v11;
    VkPhysicalDeviceExtendedDynamicState3FeaturesEXT dyn3 = {
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTENDED_DYNAMIC_STATE_3_FEATURES_EXT, g_clip ? (void *)&clipf : (void *)&v11 };
    dyn3.extendedDynamicState3PolygonMode = VK_TRUE;
    if (g_clip) dyn3.extendedDynamicState3DepthClipEnable = VK_TRUE;
    {
        PFN_vkGetPhysicalDeviceFeatures2 gpf2 = (PFN_vkGetPhysicalDeviceFeatures2)pvkGetInstanceProcAddr(g_inst, "vkGetPhysicalDeviceFeatures2");
        VkPhysicalDeviceVulkan12Features q12 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES };
        VkPhysicalDeviceCooperativeMatrixFeaturesKHR qcm = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_COOPERATIVE_MATRIX_FEATURES_KHR };
        VkPhysicalDeviceVulkan11Features q11 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_FEATURES, have[10] ? (void *)&qcm : NULL };
        q12.pNext = &q11;
        VkPhysicalDeviceFeatures2 f2 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2, &q12 };
        if (gpf2) gpf2(g_pdev, &f2);
        v12.shaderFloat16 = q12.shaderFloat16;
        v12.shaderInt8 = q12.shaderInt8;
        v12.vulkanMemoryModel = q12.vulkanMemoryModel;
        v12.vulkanMemoryModelDeviceScope = q12.vulkanMemoryModelDeviceScope;
        v11.storageBuffer16BitAccess = q11.storageBuffer16BitAccess;
        v11.uniformAndStorageBuffer16BitAccess = q11.uniformAndStorageBuffer16BitAccess;
        v11.shaderDrawParameters = q11.shaderDrawParameters;  /* [[base_vertex]] / [[base_instance]] (Blender 4.2) */
        g_float16 = q12.shaderFloat16 == VK_TRUE;
        g_vulkan_memory_model = q12.vulkanMemoryModel == VK_TRUE;
        g_vulkan_memory_device_scope = q12.vulkanMemoryModelDeviceScope == VK_TRUE;
        g_storage16 = q11.storageBuffer16BitAccess == VK_TRUE;
        g_uniform_storage16 = q11.uniformAndStorageBuffer16BitAccess == VK_TRUE;
        unsigned tile_mask = 0;
        VkPhysicalDeviceCooperativeMatrixPropertiesKHR cp = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_COOPERATIVE_MATRIX_PROPERTIES_KHR };
        if (have[10] && pvkGetPhysicalDeviceProperties2) {
            VkPhysicalDeviceProperties2 p2 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, &cp };
            pvkGetPhysicalDeviceProperties2(g_pdev, &p2);
            PFN_vkGetPhysicalDeviceCooperativeMatrixPropertiesKHR get_tiles =
                (PFN_vkGetPhysicalDeviceCooperativeMatrixPropertiesKHR)pvkGetInstanceProcAddr(g_inst, "vkGetPhysicalDeviceCooperativeMatrixPropertiesKHR");
            uint32_t count = 0;
            if (get_tiles && get_tiles(g_pdev, &count, NULL) == VK_SUCCESS && count) {
                VkCooperativeMatrixPropertiesKHR *tiles = calloc(count, sizeof *tiles);
                if (tiles) {
                    for (uint32_t i = 0; i < count; ++i) tiles[i].sType = VK_STRUCTURE_TYPE_COOPERATIVE_MATRIX_PROPERTIES_KHR;
                    if (get_tiles(g_pdev, &count, tiles) == VK_SUCCESS)
                        for (uint32_t i = 0; i < count; ++i) {
                            const VkCooperativeMatrixPropertiesKHR *t = &tiles[i];
                            if (t->MSize != 16 || t->NSize != 8 || (t->KSize != 8 && t->KSize != 16) ||
                                t->AType != VK_COMPONENT_TYPE_FLOAT16_KHR || t->BType != VK_COMPONENT_TYPE_FLOAT16_KHR ||
                                t->CType != t->ResultType || (t->CType != VK_COMPONENT_TYPE_FLOAT16_KHR && t->CType != VK_COMPONENT_TYPE_FLOAT32_KHR) ||
                                t->scope != VK_SCOPE_SUBGROUP_KHR || t->saturatingAccumulation) continue;
                            unsigned bit = (t->KSize == 16 ? 2u : 0u) + (t->CType == VK_COMPONENT_TYPE_FLOAT32_KHR ? 1u : 0u);
                            tile_mask |= 1u << bit;
                        }
                    free(tiles);
                }
            }
        }
        g_coop_matrix = have[10] && qcm.cooperativeMatrix && g_float16 && g_vulkan_memory_model &&
                        (cp.cooperativeMatrixSupportedStages & VK_SHADER_STAGE_COMPUTE_BIT) && tile_mask == 15;
        if (g_coop_matrix) { cmf.cooperativeMatrix = VK_TRUE; cmf.pNext = v12.pNext; v12.pNext = &cmf; }
        nvlog("vk: cooperative matrix %d, half arithmetic %d, storage16 %d/%d, Vulkan memory model %d/%d, tiles 0x%x",
              g_coop_matrix, g_float16, g_storage16, g_uniform_storage16, g_vulkan_memory_model, g_vulkan_memory_device_scope, tile_mask);
        v12.shaderBufferInt64Atomics = q12.shaderBufferInt64Atomics;
        v12.shaderSharedInt64Atomics = q12.shaderSharedInt64Atomics;
        g_atomic64_buffer = q12.shaderBufferInt64Atomics == VK_TRUE;
        g_atomic64_shared = q12.shaderSharedInt64Atomics == VK_TRUE;
        nvlog("vk: integer atomic64 buffer %d, shared %d", g_atomic64_buffer, g_atomic64_shared);
        v12.hostQueryReset = q12.hostQueryReset;
        g_host_query_reset = q12.hostQueryReset == VK_TRUE;
        v12.samplerMirrorClampToEdge = q12.samplerMirrorClampToEdge;
        g_sampler_mirror_clamp = q12.samplerMirrorClampToEdge == VK_TRUE;
        v12.scalarBlockLayout = q12.scalarBlockLayout; g_scalar_layout = q12.scalarBlockLayout == VK_TRUE;
        v12.shaderOutputLayer = q12.shaderOutputLayer; v12.shaderOutputViewportIndex = q12.shaderOutputViewportIndex;
        nvlog("vk: par1 shaderOutputLayer %d, shaderOutputViewportIndex %d", q12.shaderOutputLayer, q12.shaderOutputViewportIndex);
        const char *off = getenv("NVMTL_NO_BINDLESS");
        g_bindless = gpf2 && !(off && *off == '1') && q12.descriptorIndexing && q12.runtimeDescriptorArray &&
                     q12.shaderSampledImageArrayNonUniformIndexing && q12.descriptorBindingPartiallyBound &&
                     q12.descriptorBindingSampledImageUpdateAfterBind && q12.descriptorBindingUpdateUnusedWhilePending;
        if (g_bindless) {
            v12.descriptorIndexing = v12.runtimeDescriptorArray = v12.shaderSampledImageArrayNonUniformIndexing = VK_TRUE;
            v12.descriptorBindingPartiallyBound = v12.descriptorBindingSampledImageUpdateAfterBind = VK_TRUE;
            v12.descriptorBindingUpdateUnusedWhilePending = VK_TRUE;
        }
        nvlog("vk: descriptor indexing %s - bindless textures %s", g_bindless ? "present" : (off && *off == '1') ? "DISABLED by NVMTL_NO_BINDLESS" : "ABSENT",
              g_bindless ? "ON (set 2)" : "OFF");
    }
    VkPhysicalDeviceFeatures supported; pvkGetPhysicalDeviceFeatures(g_pdev, &supported);
    if (!supported.shaderInt64) { nvlog("vk: 64-bit shader addresses unavailable"); return -1; }
    VkPhysicalDeviceFeatures enabled = {0}; enabled.shaderInt64 = VK_TRUE;
    enabled.sampleRateShading = supported.sampleRateShading;
    enabled.occlusionQueryPrecise = supported.occlusionQueryPrecise;
    g_occlusion_precise = supported.occlusionQueryPrecise == VK_TRUE;
    enabled.samplerAnisotropy = supported.samplerAnisotropy;
    g_sampler_anisotropy = supported.samplerAnisotropy == VK_TRUE;
    nvlog("vk: sampler anisotropy %d (max %g), mirror-clamp-to-edge %d", g_sampler_anisotropy,
          (double)g_lim.maxSamplerAnisotropy, g_sampler_mirror_clamp);
    enabled.tessellationShader = supported.tessellationShader; g_tess = supported.tessellationShader == VK_TRUE;
    enabled.vertexPipelineStoresAndAtomics = supported.vertexPipelineStoresAndAtomics;
    enabled.independentBlend = supported.independentBlend; g_indep = supported.independentBlend == VK_TRUE;
    enabled.alphaToOne = supported.alphaToOne; g_a2one = supported.alphaToOne == VK_TRUE;
    enabled.dualSrcBlend = supported.dualSrcBlend; g_dual = supported.dualSrcBlend == VK_TRUE;
    enabled.wideLines = supported.wideLines; g_wide = supported.wideLines == VK_TRUE;
    nvlog("vk: sel1 wideLines %s (range %g..%g)", g_wide ? "AVAILABLE" : "ABSENT", (double)g_lim.lineWidthRange[0], (double)g_lim.lineWidthRange[1]);
    nvlog("vk: par2 dualSrcBlend %s", g_dual ? "AVAILABLE" : "ABSENT");
    if (pnvk_sparse && pnvk_sparse() == 1 && supported.sparseBinding) { enabled.sparseBinding = VK_TRUE; g_sparse = 1; }
    nvlog("vk: alphaToCoverage always, alphaToOne %s", g_a2one ? "AVAILABLE" : "ABSENT");
    nvlog("vk: tessellation %s, vertex-stage stores %s", g_tess ? "AVAILABLE" : "ABSENT", supported.vertexPipelineStoresAndAtomics ? "on" : "off");
    VkDeviceCreateInfo dci = { VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, g_dyn3 ? (const void *)&dyn3 : (const void *)&v11, 0,
                               1, &qci, 0, NULL, next, next ? exts : NULL, &enabled };
    VKCK(pvkCreateDevice(g_pdev, &dci, NULL, &g_dev), "vkCreateDevice");
    DEVICE_FNS(LOAD_D)
    if (g_pstat) { p_pstat_props = (PFN_vkGetPipelineExecutablePropertiesKHR)pvkGetDeviceProcAddr(g_dev, "vkGetPipelineExecutablePropertiesKHR");
                   p_pstat_stats = (PFN_vkGetPipelineExecutableStatisticsKHR)pvkGetDeviceProcAddr(g_dev, "vkGetPipelineExecutableStatisticsKHR");
                   if (!p_pstat_props || !p_pstat_stats) g_pstat = 0;
                   nvlog("pstat: per-pipeline compile statistics %s", g_pstat ? "ON (NVMTL_PSTAT=1)" : "unavailable"); }
    { extern void nvmtl_bl_hook_destroy(void); nvmtl_bl_hook_destroy(); }
    if (g_sample_locations) {
        pvkCmdSetSampleLocationsEXT = (PFN_vkCmdSetSampleLocationsEXT)pvkGetDeviceProcAddr(g_dev, "vkCmdSetSampleLocationsEXT");
        if (!pvkCmdSetSampleLocationsEXT) g_sample_locations = 0;
    }
    if (g_sample_loader_unwrapped) {
        pnvk_sample_depth_contract = (VkResult (*)(VkDevice, uint32_t *, uint32_t *))
            pvkGetDeviceProcAddr(g_dev, "vkNVMTLGetSampleDepthContractV1");
        pnvk_sample_depth_image = (VkResult (*)(VkDevice, VkImage, uint32_t *))
            pvkGetDeviceProcAddr(g_dev, "vkNVMTLVerifySampleDepthImageV1");
        nvlog("vk: sample-depth loader diagnostic unique_handles=false; synchronization/core validation retained");
    }

    if (g_sample_locations && (!pnvk_sample_depth_contract || !pnvk_sample_depth_image ||
        pnvk_sample_depth_contract(g_dev, &g_sample_depth_flags, &g_sample_depth_counts) != VK_SUCCESS ||
        (g_sample_depth_flags & NVMTL_SAMPLE_DEPTH_CONTRACT_REQUIRED) != NVMTL_SAMPLE_DEPTH_CONTRACT_REQUIRED ||
        (g_sample_depth_counts & 0xf) != 0xf)) g_sample_locations = 0;
    nvlog("vk: sample locations backend %d; Metal capability pending native qualification", g_sample_locations);
    if (have[11] && g_timestamp_bits && g_timestamp_bits <= 64 &&
        isfinite(g_lim.timestampPeriod) && g_lim.timestampPeriod > 0) {
        PFN_vkGetPhysicalDeviceCalibrateableTimeDomainsEXT domains =
            (PFN_vkGetPhysicalDeviceCalibrateableTimeDomainsEXT)pvkGetInstanceProcAddr(g_inst, "vkGetPhysicalDeviceCalibrateableTimeDomainsEXT");
        uint32_t n = 0;
        if (domains && domains(g_pdev, &n, NULL) == VK_SUCCESS && n && n <= 16) {
            VkTimeDomainEXT list[16];
            if (domains(g_pdev, &n, list) == VK_SUCCESS) {
                for (uint32_t i = 0; i < n; ++i) if (list[i] == VK_TIME_DOMAIN_DEVICE_EXT)
                    pvkGetCalibratedTimestampsEXT = (PFN_vkGetCalibratedTimestampsEXT)
                        pvkGetDeviceProcAddr(g_dev, "vkGetCalibratedTimestampsEXT");
            }
        }
    }
    pvkCmdSetLineWidth = (PFN_vkCmdSetLineWidth)pvkGetDeviceProcAddr(g_dev, "vkCmdSetLineWidth");
    if (g_wide && !pvkCmdSetLineWidth) { nvlog("vk: sel1 vkCmdSetLineWidth missing - wide lines OFF"); g_wide = 0; }
    if (g_rt) {
#define LOAD_RT(f) p##f = (PFN_##f)pvkGetDeviceProcAddr(g_dev, #f);
        LOAD_RT(vkCreateAccelerationStructureKHR) LOAD_RT(vkDestroyAccelerationStructureKHR) LOAD_RT(vkGetAccelerationStructureBuildSizesKHR)
        LOAD_RT(vkCmdBuildAccelerationStructuresKHR) LOAD_RT(vkGetAccelerationStructureDeviceAddressKHR) LOAD_RT(vkCmdCopyAccelerationStructureKHR)
        if (!pvkCreateAccelerationStructureKHR || !pvkGetAccelerationStructureBuildSizesKHR || !pvkCmdBuildAccelerationStructuresKHR ||
            !pvkGetAccelerationStructureDeviceAddressKHR) { nvlog("vk: ray tracing entry points missing — ray tracing OFF"); g_rt = 0; }
    }
    nvlog("vk: ray tracing %s", g_rt ? "AVAILABLE (acceleration structures + ray queries on NVK's software BVH)" : "ABSENT on this NVK");
    if (g_host_import) {
        pvkGetMemoryHostPointerPropertiesEXT = (PFN_vkGetMemoryHostPointerPropertiesEXT)
            pvkGetDeviceProcAddr(g_dev, "vkGetMemoryHostPointerPropertiesEXT");
        VkPhysicalDeviceExternalMemoryHostPropertiesEXT hp = {
            VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_MEMORY_HOST_PROPERTIES_EXT, NULL, 0 };
        VkPhysicalDeviceProperties2 hp2 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, &hp };
        pvkGetPhysicalDeviceProperties2(g_pdev, &hp2);
        g_host_align = (size_t)hp.minImportedHostPointerAlignment;
        if (!pvkGetMemoryHostPointerPropertiesEXT || !g_host_align || (g_host_align & (g_host_align - 1))) {
            nvlog("vk: host-pointer import OFF — entry point %s, alignment %zu (must be a power of two)",
                  pvkGetMemoryHostPointerPropertiesEXT ? "present" : "MISSING", g_host_align);
            g_host_import = 0; g_host_align = 0;
        }
    }
    nvlog("vk: newBufferWithBytesNoCopy: %s", g_host_import
          ? "imports the caller's pages (VK_EXT_external_memory_host)" : "must COPY — no host-pointer import on this device");
    if (g_dyn3) {
        VkPhysicalDeviceExtendedDynamicState3PropertiesEXT p3 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTENDED_DYNAMIC_STATE_3_PROPERTIES_EXT };
        VkPhysicalDeviceProperties2 p2 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, &p3 };
        pvkGetPhysicalDeviceProperties2(g_pdev, &p2); g_topo_any = p3.dynamicPrimitiveTopologyUnrestricted ? 1 : 0;
    }
    nvlog("vk: G14 primitive topology is dynamic; points/lines on a triangle-class pipeline: %s", g_topo_any ? "yes" : "NO");
    if (g_dyn3) {
        pvkCmdSetPolygonModeEXT = (PFN_vkCmdSetPolygonModeEXT)pvkGetDeviceProcAddr(g_dev, "vkCmdSetPolygonModeEXT");
        if (!pvkCmdSetPolygonModeEXT) { nvlog("vk: the extension is enabled but vkCmdSetPolygonModeEXT is missing — fill mode stays static"); g_dyn3 = 0; }
    }
    if (g_clip) {
        pvkCmdSetDepthClipEnableEXT = (PFN_vkCmdSetDepthClipEnableEXT)pvkGetDeviceProcAddr(g_dev, "vkCmdSetDepthClipEnableEXT");
        if (!pvkCmdSetDepthClipEnableEXT) { nvlog("vk: vkCmdSetDepthClipEnableEXT is missing — depth clip mode stays static"); g_clip = 0; }
    }
    pvkCmdBindVertexBuffers2 = (PFN_vkCmdBindVertexBuffers2)pvkGetDeviceProcAddr(g_dev, "vkCmdBindVertexBuffers2");
    pvkCreateRenderPass2 = (PFN_vkCreateRenderPass2)pvkGetDeviceProcAddr(g_dev, "vkCreateRenderPass2");
    pvkCreateBufferView = (PFN_vkCreateBufferView)pvkGetDeviceProcAddr(g_dev, "vkCreateBufferView");
    pvkDestroyBufferView = (PFN_vkDestroyBufferView)pvkGetDeviceProcAddr(g_dev, "vkDestroyBufferView");
    { VkPhysicalDeviceDepthStencilResolveProperties dsp = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DEPTH_STENCIL_RESOLVE_PROPERTIES };
      VkPhysicalDeviceProperties2 pp2 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, &dsp };
      pvkGetPhysicalDeviceProperties2(g_pdev, &pp2);
      g_dsr_depth_modes = dsp.supportedDepthResolveModes; g_dsr_stencil_modes = dsp.supportedStencilResolveModes;
      g_dsr_indep = dsp.independentResolve; g_dsr_indep_none = dsp.independentResolveNone;
      nvlog("vk: par1 vkCreateRenderPass2 %s; depth resolve modes 0x%x, stencil 0x%x, independent %u/%u", pvkCreateRenderPass2 ? "loaded" : "ABSENT",
            g_dsr_depth_modes, g_dsr_stencil_modes, g_dsr_indep, g_dsr_indep_none); }
    nvlog("vk: apps-dyn dynamic vertex strides (vkCmdBindVertexBuffers2): %s", pvkCmdBindVertexBuffers2 ? "yes" : "NO — MTLBufferLayoutStrideDynamic pipelines are refused");
    pvkGetDeviceQueue(g_dev, g_qfam, 0, &g_queue);

    g_state = 1;
    nvlog("vk: UP in this Metal process — \"%s\" api %u.%u.%u, queue family %u", g_name,
          VK_VERSION_MAJOR(pp.apiVersion), VK_VERSION_MINOR(pp.apiVersion), VK_VERSION_PATCH(pp.apiVersion), g_qfam);
    return 0;
}
static pthread_once_t g_vk_once = PTHREAD_ONCE_INIT;
static void nvmtl_vk_initialize_once(void) { (void)nvmtl_vk_initialize(); }
int nvmtl_vk_init(void) {
    pthread_once(&g_vk_once, nvmtl_vk_initialize_once);
    return g_state == 1 ? 0 : -1;
}
const char *nvmtl_vk_device_name(void) { return g_state == 1 ? g_name : "(vulkan down)"; }
void nvmtl_vk_limits(uint32_t *layers, uint32_t *shared_bytes, uint32_t wg[3], uint32_t *wg_inv, uint32_t *sm) {
    int up = g_state == 1;
    if (layers) *layers = up ? g_lim.maxImageArrayLayers : 0;
    if (shared_bytes) *shared_bytes = up ? g_lim.maxComputeSharedMemorySize : 0;
    if (wg) { wg[0] = up ? g_lim.maxComputeWorkGroupSize[0] : 0; wg[1] = up ? g_lim.maxComputeWorkGroupSize[1] : 0; wg[2] = up ? g_lim.maxComputeWorkGroupSize[2] : 0; }
    if (wg_inv) *wg_inv = up ? g_lim.maxComputeWorkGroupInvocations : 0;
    if (sm) *sm = up ? g_sm_count : 0;
}

void nvmtl_vk_pubcaps(uint32_t *color_samples, uint32_t *depth_samples, uint64_t *max_storage_range, uint64_t *linear_align) {
    int up = g_state == 1;
    if (color_samples) *color_samples = up ? (uint32_t)(g_lim.framebufferColorSampleCounts & g_lim.sampledImageColorSampleCounts) : 0;
    if (depth_samples) *depth_samples = up ? (uint32_t)(g_lim.framebufferDepthSampleCounts & g_lim.sampledImageDepthSampleCounts) : 0;
    if (max_storage_range) *max_storage_range = up ? (uint64_t)g_lim.maxStorageBufferRange : 0;
    if (linear_align) {
        uint64_t a = up ? (uint64_t)g_lim.optimalBufferCopyRowPitchAlignment : 0;
        uint64_t t = up ? (uint64_t)g_lim.minTexelBufferOffsetAlignment : 0;
        *linear_align = a > t ? a : t;
    }
}

static int memtype(uint32_t bits, VkMemoryPropertyFlags want)
{
    if (want & VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT)
        for (uint32_t i = 0; i < g_memp.memoryTypeCount; i++)
            if ((bits & (1u << i)) && (g_memp.memoryTypes[i].propertyFlags & want) == want
                && !(g_memp.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) return (int)i;
    for (uint32_t i = 0; i < g_memp.memoryTypeCount; i++)
        if ((bits & (1u << i)) && (g_memp.memoryTypes[i].propertyFlags & want) == want) return (int)i;
    return -1;
}

#define NVMTL_POOL_BLOCK (32u << 20)
#define NVMTL_POOL_MAX   (4u << 20)
#define NVMTL_POOL_KEEP  2
static size_t g_bar1_bytes; static int g_bar1_off;
static int bar1_on(void)
{
    static int env = -1;
    if (env < 0) { const char *e = getenv("NVMTL_SHARED_VRAM"); env = e && e[0] == '1';
        if (!env) nvlog("vk: shared-in-VRAM off - Shared stays in system RAM at every size (batch 56 default; NVMTL_SHARED_VRAM=1 opts in)"); }
    return env;
}
static int bar1_memtype(uint32_t bits, VkDeviceSize size)
{
    if (g_bar1_off) return -1;
    if (!bar1_on()) { g_bar1_off = 1; return -1; }
    const VkMemoryPropertyFlags want = VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT | VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
    for (uint32_t i = 0; i < g_memp.memoryTypeCount; i++) {
        if (!(bits & (1u << i)) || (g_memp.memoryTypes[i].propertyFlags & want) != want) continue;
        VkDeviceSize heap = g_memp.memoryHeaps[g_memp.memoryTypes[i].heapIndex].size;
        VkDeviceSize budget = heap > (256ull << 20) ? heap - (256ull << 20) : 0;
        { static long long cap = -2; if (cap == -2) { const char *e = getenv("NVMTL_SHARED_VRAM_BUDGET_MB"); cap = (e && *e) ? atoll(e) : -1; }
          if (cap >= 0 && ((VkDeviceSize)cap << 20) < budget) budget = (VkDeviceSize)cap << 20; }
        { static int said; if (!said++) nvlog("vk: shared-in-VRAM: memory type %u flags 0x%x heap %u of %llu MB, budget %llu MB",
              i, g_memp.memoryTypes[i].propertyFlags, g_memp.memoryTypes[i].heapIndex, (unsigned long long)(heap >> 20), (unsigned long long)(budget >> 20)); }
        if (g_bar1_bytes + size > budget) { static int full; if (!full++) nvlog("vk: shared-in-VRAM budget full (%zu + %llu > %llu bytes) - system RAM from here", g_bar1_bytes, (unsigned long long)size, (unsigned long long)budget); return -1; }
        return (int)i;
    }
    { static int said; if (!said++) nvlog("vk: shared-in-VRAM: no DEVICE_LOCAL|HOST_VISIBLE|HOST_COHERENT type in bits 0x%x", bits); }
    return -1;
}
static int  pool_buffer_create(size_t size, nvk_buffer *out);
static int  pool_buffer_create_in(int vram, size_t size, nvk_buffer *out);
static int  vpool_on(void);
static void pool_buffer_release(nvk_buffer *b);
#define NVMTL_BUFPARK_N     32
#define NVMTL_BUFPARK_BYTES (256ull << 20)
#define NVMTL_BUFPARK_MAX1  (64ull << 20)
static pthread_mutex_t g_bpark_lock = PTHREAD_MUTEX_INITIALIZER;
static nvk_buffer g_bpark[NVMTL_BUFPARK_N];
static unsigned g_bpark_n;
static uint64_t g_bpark_bytes;
static _Atomic uint64_t g_bpark_parked, g_bpark_reused, g_bpark_evicted, g_bpark_drained;
static int bpark_on(void)
{
    static int on = -1;
    if (on < 0) { on = !getenv("NVMTL_NO_BUFREUSE");
        nvlog("buffer park: %s (NVMTL_NO_BUFREUSE=%s)", on ? "a dead MTLBuffer's memory is reused by the next create of its exact size and placement"
              : "OFF - every dead buffer is freed", getenv("NVMTL_NO_BUFREUSE") ?: "unset"); }
    return on;
}
static int bpark_kind(const nvk_buffer *b) { return b->bar1 ? 2 : b->map ? 1 : 0; }
static void bpark_census(const char *what)
{
    const uint64_t n = atomic_load(&g_bpark_parked) + atomic_load(&g_bpark_reused);
    if (n & (n - 1)) return;
    nvlog("buffer park (%s): %llu parked, %llu reused, %llu evicted, %llu drained; %u held, %llu bytes", what,
          (unsigned long long)atomic_load(&g_bpark_parked), (unsigned long long)atomic_load(&g_bpark_reused),
          (unsigned long long)atomic_load(&g_bpark_evicted), (unsigned long long)atomic_load(&g_bpark_drained),
          g_bpark_n, (unsigned long long)g_bpark_bytes);
}
static void bpark_free(nvk_buffer *b)
{
    pvkDestroyBuffer(g_dev, (VkBuffer)b->buf, NULL);
    if (b->map && pvkUnmapMemory) pvkUnmapMemory(g_dev, (VkDeviceMemory)b->mem);
    if (b->bar1) g_bar1_bytes -= b->alloc;
    pvkFreeMemory(g_dev, (VkDeviceMemory)b->mem, NULL); g_alloc_bytes -= b->alloc;
    memset(b, 0, sizeof *b);
}
static int bpark_take(size_t size, int host_visible, nvk_buffer *out)
{
    if (!bpark_on()) return -1;
    const int want = host_visible == 2 ? 2 : host_visible ? 1 : 0;
    int hit = -1; nvk_buffer got;
    pthread_mutex_lock(&g_bpark_lock);
    for (unsigned k = g_bpark_n; k-- > 0; ) {
        const int kind = bpark_kind(&g_bpark[k]);
        if (g_bpark[k].size == size && (kind == want || (want == 2 && kind == 1 && g_bar1_off))) { hit = (int)k; break; } }
    if (hit >= 0) {
        got = g_bpark[hit]; g_bpark_bytes -= got.alloc;
        memmove(&g_bpark[hit], &g_bpark[hit + 1], (g_bpark_n - (unsigned)hit - 1) * sizeof got); g_bpark_n--;
    }
    pthread_mutex_unlock(&g_bpark_lock);
    if (hit < 0) return -1;
    *out = got; g_buf_made++;
    atomic_fetch_add(&g_bpark_reused, 1); bpark_census("reuse");
    return 0;
}
static void bpark_drain_private(void)
{
    nvk_buffer out[NVMTL_BUFPARK_N]; unsigned nout = 0;
    pthread_mutex_lock(&g_bpark_lock);
    unsigned w = 0;
    for (unsigned k = 0; k < g_bpark_n; k++) {
        if (bpark_kind(&g_bpark[k]) == 0) { g_bpark_bytes -= g_bpark[k].alloc; out[nout++] = g_bpark[k]; }
        else g_bpark[w++] = g_bpark[k]; }
    g_bpark_n = w;
    pthread_mutex_unlock(&g_bpark_lock);
    for (unsigned k = 0; k < nout; k++) bpark_free(&out[k]);
    if (nout) { atomic_fetch_add(&g_bpark_drained, nout);
        nvlog("buffer park: VRAM wall refused - drained %u parked Private buffers", nout); }
}
static int nvmtl_vk_buffer_create_body(size_t size, int host_visible, nvk_buffer *out);
static pthread_mutex_t nvmtl_submit_mutex;
static uint64_t g_res3_made, g_res3_madesys, g_res3_off, g_res3_on, g_res3_off_b, g_res3_on_b;
void nvmtl_vk_res3_stats(uint64_t out[6]) { out[0] = g_res3_made; out[1] = g_res3_madesys; out[2] = g_res3_off; out[3] = g_res3_on;
    out[4] = g_res3_off_b >> 20; out[5] = g_res3_on_b >> 20; }
static int nvmtl_vk_sparse_on(void)
{
    static int on = -1;
    if (on < 0) {
        const char *e = getenv("NVMTL_NO_SPARSE_BUF"), *r = getenv("NVMTL_NO_RES2");
        on = g_sparse && g_qsparse && !(e && e[0] == '1') && !(r && r[0] == '1');
        nvlog("residency (res3): Private buffers >= 1 MiB %s (NVK sparse bind %s, queue sparse %s, NVMTL_NO_SPARSE_BUF=%s)",
              on ? "get a STABLE GPU address whose memory pages" : "as before (fixed memory)",
              g_sparse ? "present" : "ABSENT", g_qsparse ? "yes" : "no", e ? e : "unset");
    }
    return on;
}
static int nvmtl_vk_sparse_bind(VkBuffer b, int unbind_first, VkDeviceMemory mem, VkDeviceSize size)
{
    VkSparseMemoryBind mb[2]; uint32_t n = 0;
    if (unbind_first) mb[n++] = (VkSparseMemoryBind){ 0, size, VK_NULL_HANDLE, 0, 0 };
    if (mem) mb[n++] = (VkSparseMemoryBind){ 0, size, mem, 0, 0 };
    VkSparseBufferMemoryBindInfo bbi = { b, n, mb };
    VkBindSparseInfo bsi = { VK_STRUCTURE_TYPE_BIND_SPARSE_INFO, NULL, 0, NULL, 1, &bbi, 0, NULL, 0, NULL, 0, NULL };
    VkFenceCreateInfo fci = { VK_STRUCTURE_TYPE_FENCE_CREATE_INFO, NULL, 0 }; VkFence f = VK_NULL_HANDLE;
    if (pvkCreateFence(g_dev, &fci, NULL, &f) != VK_SUCCESS) return -1;
    pthread_mutex_lock(&nvmtl_submit_mutex);
    VkResult r = g_state == 1 ? pvkQueueBindSparse(g_queue, 1, &bsi, f) : VK_ERROR_DEVICE_LOST;
    pthread_mutex_unlock(&nvmtl_submit_mutex);
    if (r == VK_SUCCESS) r = pvkWaitForFences(g_dev, 1, &f, VK_TRUE, 5000000000ull);
    pvkDestroyFence(g_dev, f, NULL);
    if (r != VK_SUCCESS) nvlog("residency (res3): vkQueueBindSparse(%s%s, %llu MB) -> %d", unbind_first ? "unbind + " : "",
                               mem ? "bind" : "nothing", (unsigned long long)(size >> 20), r);
    return r == VK_SUCCESS ? 0 : -1;
}
static int nvmtl_vk_sysmem_type(uint32_t bits)
{
    for (uint32_t k = 0; k < g_memp.memoryTypeCount; k++)
        if ((bits & (1u << k)) && !(g_memp.memoryTypes[k].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) return (int)k;
    return -1;
}
static int nvmtl_vk_buffer_create_sparse(size_t size, nvk_buffer *out)
{
    VkBufferCreateInfo bci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, VK_BUFFER_CREATE_SPARSE_BINDING_BIT, size,
        VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT | VK_BUFFER_USAGE_VERTEX_BUFFER_BIT |
        VK_BUFFER_USAGE_INDEX_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT |
        VK_BUFFER_USAGE_INDIRECT_BUFFER_BIT | VK_BUFFER_USAGE_SHADER_DEVICE_ADDRESS_BIT | NVMTL_RT_BUFFER_USAGE,
        VK_SHARING_MODE_EXCLUSIVE, 0, NULL };
    VkBuffer b = VK_NULL_HANDLE;
    if (pvkCreateBuffer(g_dev, &bci, NULL, &b) != VK_SUCCESS) return -1;
    VkMemoryRequirements mr; pvkGetBufferMemoryRequirements(g_dev, b, &mr);
    uint64_t bfree = 0, bbud = 0, bused = 0; int bmeas = 0;
    int sys = nvmtl_vk_vram_wall(mr.size, &bfree, &bbud, &bused, &bmeas);
    VkMemoryAllocateFlagsInfo mafi = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO, NULL, VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT, 0 };
    VkMemoryAllocateInfo mai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &mafi, mr.size, 0 };
    VkDeviceMemory m = VK_NULL_HANDLE; VkResult ar = VK_ERROR_OUT_OF_DEVICE_MEMORY;
    if (!sys) { int mt = memtype(mr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
                if (mt >= 0) { mai.memoryTypeIndex = (uint32_t)mt; ar = pvkAllocateMemory(g_dev, &mai, NULL, &m); } }
    if (ar != VK_SUCCESS) { int mt = nvmtl_vk_sysmem_type(mr.memoryTypeBits); sys = 1;
                            if (mt >= 0) { mai.memoryTypeIndex = (uint32_t)mt; ar = pvkAllocateMemory(g_dev, &mai, NULL, &m); } }
    if (ar != VK_SUCCESS || nvmtl_vk_sparse_bind(b, 0, m, mr.size)) {
        if (m) pvkFreeMemory(g_dev, m, NULL);
        pvkDestroyBuffer(g_dev, b, NULL);
        static unsigned n; if (n++ < 4) nvlog("residency (res3): a %zu-byte stable-address buffer was refused (alloc %d) - the old path", size, ar);
        return -1;
    }
    out->buf = b; out->mem = m; out->size = size; out->alloc = mr.size; out->sparse = 1; out->sysmem = sys;
    g_alloc_bytes += mr.size; g_buf_made++; g_res3_made++;
    if (!sys) nvmtl_vk_budget_charge(mr.size);
    else { g_res3_madesys++; g_buf_vram_refused++;
           nvlog("vk: private buffer of %zu bytes REFUSED VRAM (our soft gate) -> system memory under a stable address (res3): %llu MB free, "
                 "heapUsage %llu of budget %llu MB, %llu refused so far", size, (unsigned long long)(bfree >> 20),
                 (unsigned long long)(bused >> 20), (unsigned long long)(bbud >> 20), (unsigned long long)g_buf_vram_refused); }
    return 0;
}

int nvmtl_vk_buffer_create(size_t size, int host_visible, nvk_buffer *out)
{
    const uint64_t t0 = nvmtl_perf_now();
    nvk_buffer pending = {0};
    const int r = nvmtl_vk_buffer_create_body(size, host_visible, &pending);
    if (r) {
        if (pending.buf) pvkDestroyBuffer(g_dev, (VkBuffer)pending.buf, NULL);
        if (pending.mem) {
            if (pending.map) pvkUnmapMemory(g_dev, (VkDeviceMemory)pending.mem);
            pvkFreeMemory(g_dev, (VkDeviceMemory)pending.mem, NULL);
            if (!host_visible) g_budget_stale = 1;
        }
        memset(out, 0, sizeof *out);
    } else *out = pending;
    nvmtl_perf_note(r == 0 && out->pool ? NVP_BUFPOOL : NVP_BUFNEW, t0, size);
    return r;
}
static int nvmtl_vk_buffer_create_body(size_t size, int host_visible, nvk_buffer *out)
{
    if (nvmtl_vk_init()) return -1;
    memset(out, 0, sizeof *out); out->size = size;
    if (host_visible == 2 && size <= NVMTL_POOL_MAX && vpool_on() && pool_buffer_create_in(1, size, out) == 0) return 0;
    memset(out, 0, sizeof *out); out->size = size;
    if (host_visible && size <= NVMTL_POOL_MAX && pool_buffer_create(size, out) == 0) return 0;
    memset(out, 0, sizeof *out); out->size = size;
    if (bpark_take(size, host_visible, out) == 0) return 0;
    memset(out, 0, sizeof *out); out->size = size;
    if (!host_visible && size >= (1u << 20) && nvmtl_vk_sparse_on() && nvmtl_vk_buffer_create_sparse(size, out) == 0) return 0;
    memset(out, 0, sizeof *out); out->size = size;
    VkBufferCreateInfo bci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, 0, size,
        VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT | VK_BUFFER_USAGE_VERTEX_BUFFER_BIT |
        VK_BUFFER_USAGE_INDEX_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT |
        VK_BUFFER_USAGE_INDIRECT_BUFFER_BIT | VK_BUFFER_USAGE_SHADER_DEVICE_ADDRESS_BIT | NVMTL_RT_BUFFER_USAGE,
        VK_SHARING_MODE_EXCLUSIVE, 0, NULL };
    VKCK(nvmtl_opaque_CreateBuffer(g_dev, &bci, NULL, &out->buf), "vkCreateBuffer");
    VkMemoryRequirements mr; pvkGetBufferMemoryRequirements(g_dev, out->buf, &mr);
    int mt = memtype(mr.memoryTypeBits, host_visible
        ? (VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) : VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    if (mt < 0) { nvlog("vk: no memory type for buffer (host_visible=%d)", host_visible); return -1; }
    if (host_visible == 2 && size > NVMTL_POOL_MAX) { int bt = bar1_memtype(mr.memoryTypeBits, mr.size); if (bt >= 0) {
        VkMemoryAllocateFlagsInfo bf = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO, NULL, VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT, 0 };
        VkMemoryAllocateInfo bi = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &bf, mr.size, (uint32_t)bt };
        VkDeviceMemory bm = VK_NULL_HANDLE; void *bp = NULL;
        VkResult r1 = pvkAllocateMemory(g_dev, &bi, NULL, &bm);
        VkResult r2 = r1 == VK_SUCCESS ? pvkMapMemory(g_dev, bm, 0, VK_WHOLE_SIZE, 0, &bp) : r1;
        VkResult r3 = r2 == VK_SUCCESS ? pvkBindBufferMemory(g_dev, out->buf, bm, 0) : r2;
        if (r3 == VK_SUCCESS) {
            out->mem = bm; out->map = bp; out->alloc = mr.size; out->bar1 = 1; g_alloc_bytes += mr.size; g_bar1_bytes += mr.size; g_buf_made++;
            { static int said; if (!said++) nvlog("vk: shared buffer of %zu bytes placed in VRAM through BAR1 (type %d)", size, bt); }
            return 0; }
        if (bp) pvkUnmapMemory(g_dev, bm);
        if (bm) pvkFreeMemory(g_dev, bm, NULL);
        g_bar1_off = 1;
        nvlog("vk: shared-in-VRAM REFUSED (alloc %d, map %d, bind %d) at %zu BAR bytes - system RAM for this process", r1, r2, r3, g_bar1_bytes); } }
    VkMemoryAllocateFlagsInfo mafi = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO, NULL,
                                       VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT, 0 };
    VkMemoryAllocateInfo mai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &mafi, mr.size, (uint32_t)mt };
    if (!host_visible) {
        uint64_t bbud = 0, bused = 0, bfree = 0; int bmeas = 0;
        int bgate = nvmtl_vk_vram_wall(mr.size, &bfree, &bbud, &bused, &bmeas);
        VkResult br = bgate ? VK_ERROR_OUT_OF_DEVICE_MEMORY
                            : nvmtl_opaque_AllocateMemory(g_dev, &mai, NULL, &out->mem);
        if (br != VK_SUCCESS) {
            g_buf_vram_refused++;
            bpark_drain_private();
            nvlog("vk: private buffer of %zu bytes REFUSED VRAM (%s) -> %d: %llu MB free, %llu MB headroom, "
                  "heapUsage %llu of budget %llu MB (%s), %llu buffers live, %llu refused so far "
                  "- the caller places it in system memory",
                  size, bgate ? "our soft gate" : "RM refused it", br,
                  (unsigned long long)(bfree >> 20), (unsigned long long)(NVMTL_VRAM_HEADROOM >> 20),
                  (unsigned long long)(bused >> 20), (unsigned long long)(bbud >> 20),
                  bmeas ? "measured" : "estimated",
                  (unsigned long long)(g_buf_made - g_buf_gone),
                  (unsigned long long)g_buf_vram_refused);
            return -1;
        }
    } else VKCK(nvmtl_opaque_AllocateMemory(g_dev, &mai, NULL, &out->mem), "vkAllocateMemory(buffer)");
    out->alloc = mr.size;
    if (!host_visible) nvmtl_vk_budget_charge(mr.size);
    VKCK(pvkBindBufferMemory(g_dev, out->buf, out->mem, 0), "vkBindBufferMemory");
    if (host_visible) { VkResult mr_ = pvkMapMemory(g_dev, out->mem, 0, VK_WHOLE_SIZE, 0, &out->map); if (mr_ != VK_SUCCESS) {
        nvlog("vk: vkMapMemory -> %d with %llu buffers live (%llu made, %llu gone), %zu bytes allocated, memory type %d", mr_,
              (unsigned long long)(g_buf_made - g_buf_gone), (unsigned long long)g_buf_made, (unsigned long long)g_buf_gone, g_alloc_bytes, mt);
        return -1; } }
    g_alloc_bytes += mr.size;
    g_buf_made++;
    if ((g_buf_made & 0xFFF) == 0) nvlog("buffer census: %llu made, %llu gone, %llu live, %zu bytes allocated; pool %llu blocks, %llu pooled live, %llu bytes in ranges",
        (unsigned long long)g_buf_made, (unsigned long long)g_buf_gone, (unsigned long long)(g_buf_made - g_buf_gone), g_alloc_bytes,
        (unsigned long long)g_pool_blocks, (unsigned long long)g_pool_live, (unsigned long long)g_pool_bytes);
    { static int said[2]; if (!said[!!host_visible]) { said[!!host_visible] = 1;
        nvlog("vk: buffers (host_visible=%d) take memory type %d flags 0x%x heap %u", host_visible, mt,
              g_memp.memoryTypes[mt].propertyFlags, g_memp.memoryTypes[mt].heapIndex); } }
    return 0;
}

size_t nvmtl_vk_host_import_align(void) { return g_host_align; }
const char *nvmtl_image_ident(const void *addr, char *out, size_t n)
{
    Dl_info di;
    if (!out || !n) return "";
    snprintf(out, n, "?");
    if (!addr || !dladdr(addr, &di) || !di.dli_fbase) return out;
    const char *path = di.dli_fname ? di.dli_fname : "?";
    const struct mach_header_64 *mh = (const struct mach_header_64 *)di.dli_fbase;
    if (mh->magic != MH_MAGIC_64) { snprintf(out, n, "not-64-bit %s", path); return out; }
    const struct load_command *lc = (const struct load_command *)(mh + 1);
    for (uint32_t i = 0; i < mh->ncmds && lc->cmdsize; i++, lc = (const struct load_command *)((const char *)lc + lc->cmdsize)) {
        if (lc->cmd != LC_UUID) continue;
        const uint8_t *u = ((const struct uuid_command *)lc)->uuid;
        snprintf(out, n, "%02X%02X%02X%02X-%02X%02X-%02X%02X-%02X%02X-%02X%02X%02X%02X%02X%02X %s", u[0], u[1], u[2], u[3], u[4], u[5],
                 u[6], u[7], u[8], u[9], u[10], u[11], u[12], u[13], u[14], u[15], path);
        return out;
    }
    snprintf(out, n, "no-uuid %s", path);
    return out;
}

int nvmtl_vk_buffer_import_host(void *ptr, size_t size, nvk_buffer *out)
{
    if (nvmtl_vk_init()) return -1;
    memset(out, 0, sizeof *out);
    if (!g_host_import || !pvkGetMemoryHostPointerPropertiesEXT || !g_host_align || !ptr || !size) return -1;
    if (((uintptr_t)ptr & (g_host_align - 1)) || (size & (g_host_align - 1))) {
        static int said; if (!said++) nvlog("vk: host-pointer import refused — ptr %p / size %zu are not both a multiple of %zu; that buffer is COPIED",
                                            ptr, size, g_host_align);
        return -1;
    }
    VkMemoryHostPointerPropertiesEXT mhp = { VK_STRUCTURE_TYPE_MEMORY_HOST_POINTER_PROPERTIES_EXT, NULL, 0 };
    if (pvkGetMemoryHostPointerPropertiesEXT(g_dev, VK_EXTERNAL_MEMORY_HANDLE_TYPE_HOST_ALLOCATION_BIT_EXT, ptr, &mhp) != VK_SUCCESS
        || !mhp.memoryTypeBits) { nvlog("vk: this pointer reports no importable memory type"); return -1; }
    out->size = size;
    VkExternalMemoryBufferCreateInfo emb = { VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_BUFFER_CREATE_INFO, NULL,
                                             VK_EXTERNAL_MEMORY_HANDLE_TYPE_HOST_ALLOCATION_BIT_EXT };
    VkBufferCreateInfo bci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, &emb, 0, size,
        VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT | VK_BUFFER_USAGE_VERTEX_BUFFER_BIT |
        VK_BUFFER_USAGE_INDEX_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT |
        VK_BUFFER_USAGE_INDIRECT_BUFFER_BIT | VK_BUFFER_USAGE_SHADER_DEVICE_ADDRESS_BIT | NVMTL_RT_BUFFER_USAGE,
        VK_SHARING_MODE_EXCLUSIVE, 0, NULL };
    if (nvmtl_opaque_CreateBuffer(g_dev, &bci, NULL, &out->buf) != VK_SUCCESS) {
        nvlog("vk: vkCreateBuffer refused an external-memory buffer of %zu bytes", size); out->buf = NULL; return -1; }
    VkMemoryRequirements mr; pvkGetBufferMemoryRequirements(g_dev, (VkBuffer)out->buf, &mr);
    int mt = memtype(mr.memoryTypeBits & mhp.memoryTypeBits,
                     VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    if (mt < 0) { nvlog("vk: no host-visible memory type is both importable and valid for this buffer");
                  pvkDestroyBuffer(g_dev, (VkBuffer)out->buf, NULL); out->buf = NULL; return -1; }
    VkMemoryAllocateFlagsInfo mafi = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO, NULL,
                                       VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT, 0 };
    VkImportMemoryHostPointerInfoEXT imp = { VK_STRUCTURE_TYPE_IMPORT_MEMORY_HOST_POINTER_INFO_EXT, &mafi,
                                             VK_EXTERNAL_MEMORY_HANDLE_TYPE_HOST_ALLOCATION_BIT_EXT, ptr };
    VkMemoryAllocateInfo mai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &imp, size, (uint32_t)mt };
    if (nvmtl_opaque_AllocateMemory(g_dev, &mai, NULL, &out->mem) != VK_SUCCESS) {
        nvlog("vk: vkAllocateMemory refused the imported host pointer %p (%zu bytes, memory type %d)", ptr, size, mt);
        pvkDestroyBuffer(g_dev, (VkBuffer)out->buf, NULL); out->buf = NULL; out->mem = NULL; return -1; }
    if (pvkBindBufferMemory(g_dev, (VkBuffer)out->buf, (VkDeviceMemory)out->mem, 0) != VK_SUCCESS) {
        nvlog("vk: vkBindBufferMemory refused the imported allocation");
        pvkFreeMemory(g_dev, (VkDeviceMemory)out->mem, NULL); pvkDestroyBuffer(g_dev, (VkBuffer)out->buf, NULL);
        out->buf = NULL; out->mem = NULL; return -1; }
    out->map = ptr; out->alloc = size; out->imported = 1;
    g_alloc_bytes += size; g_buf_made++;
    { static int said; if (!said++) nvlog("vk: imported the caller's pages as device memory — %zu bytes at %p, memory type %d", size, ptr, mt); }
    return 0;
}

static pthread_key_t g_stage_key; static pthread_once_t g_stage_once = PTHREAD_ONCE_INIT;
static void nvmtl_stage_free(void *p) { nvk_buffer *b = p; nvmtl_vk_buffer_destroy(b); free(b); }
static void nvmtl_stage_key_make(void) { if (pthread_key_create(&g_stage_key, nvmtl_stage_free)) nvlog("stage: pthread_key_create FAILED"); }
static int nvmtl_vk_stage(nvk_queue *q, size_t size, nvk_buffer *out)
{
    (void)q; pthread_once(&g_stage_once, nvmtl_stage_key_make);
    nvk_buffer *s = pthread_getspecific(g_stage_key);
    if (!s) { s = calloc(1, sizeof *s); if (!s) return -1; if (pthread_setspecific(g_stage_key, s)) { free(s); nvlog("stage: pthread_setspecific FAILED"); return -1; } }
    if (s->buf && s->size >= size) { *out = *s; return 0; }
    nvmtl_vk_buffer_destroy(s); memset(s, 0, sizeof *s);
    if (nvmtl_vk_buffer_create(size, 1, s)) { memset(s, 0, sizeof *s); return -1; }
    *out = *s;
    return 0;
}

int nvmtl_vk_format_is_storage(uint32_t vkfmt)
{
    static void (*fp)(VkPhysicalDevice, VkFormat, VkFormatProperties *); static int tried;
    if (nvmtl_vk_init()) return 0;
    if (!tried) { tried = 1; fp = (void (*)(VkPhysicalDevice, VkFormat, VkFormatProperties *))pvkGetInstanceProcAddr(g_inst, "vkGetPhysicalDeviceFormatProperties");
                  if (!fp) nvlog("vk: vkGetPhysicalDeviceFormatProperties is missing — NO texture can be a storage image"); }
    if (!fp) return 0;
    VkFormatProperties fpz; memset(&fpz, 0, sizeof fpz); fp(g_pdev, (VkFormat)vkfmt, &fpz);
    return (fpz.optimalTilingFeatures & VK_FORMAT_FEATURE_STORAGE_IMAGE_BIT) != 0;
}
static int nvmtl_vk_cube_compat(uint32_t mtl_type, uint32_t w, uint32_t h, uint32_t layers)
{ return mtl_type == 3 && w == h && w <= 0x8000 && layers >= 6; }
static uint32_t nvmtl_vk_srgb_pair(uint32_t f)
{
    switch (f) {
        case VK_FORMAT_R8_UNORM: return VK_FORMAT_R8_SRGB;                          case VK_FORMAT_R8_SRGB: return VK_FORMAT_R8_UNORM;
        case VK_FORMAT_R8G8_UNORM: return VK_FORMAT_R8G8_SRGB;                      case VK_FORMAT_R8G8_SRGB: return VK_FORMAT_R8G8_UNORM;
        case VK_FORMAT_R8G8B8A8_UNORM: return VK_FORMAT_R8G8B8A8_SRGB;              case VK_FORMAT_R8G8B8A8_SRGB: return VK_FORMAT_R8G8B8A8_UNORM;
        case VK_FORMAT_B8G8R8A8_UNORM: return VK_FORMAT_B8G8R8A8_SRGB;              case VK_FORMAT_B8G8R8A8_SRGB: return VK_FORMAT_B8G8R8A8_UNORM;
        case VK_FORMAT_A8B8G8R8_UNORM_PACK32: return VK_FORMAT_A8B8G8R8_SRGB_PACK32; case VK_FORMAT_A8B8G8R8_SRGB_PACK32: return VK_FORMAT_A8B8G8R8_UNORM_PACK32;
        default: return 0;
    }
}
static int nvmtl_vk_3d_slice_ok(VkFormat fmt, VkImageUsageFlags usage)
{
    static PFN_vkGetPhysicalDeviceImageFormatProperties2 gif; static int tried;
    if (!tried) { tried = 1;
        gif = (PFN_vkGetPhysicalDeviceImageFormatProperties2)pvkGetInstanceProcAddr(g_inst, "vkGetPhysicalDeviceImageFormatProperties2");
        if (!gif) nvlog("vk: vkGetPhysicalDeviceImageFormatProperties2 is MISSING — no 3D texture can take a per-slice attachment view"); }
    if (!gif) return 0;
    VkPhysicalDeviceImageFormatInfo2 in = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_FORMAT_INFO_2, NULL, fmt,
        VK_IMAGE_TYPE_3D, VK_IMAGE_TILING_OPTIMAL, usage,
        VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT | VK_IMAGE_CREATE_2D_ARRAY_COMPATIBLE_BIT };
    VkImageFormatProperties2 props = { VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2 };
    VkResult r = gif(g_pdev, &in, &props);
    static int said;
    if (r != VK_SUCCESS && said++ < 4)
        nvlog("vk: a 3D VkFormat %u usage 0x%x cannot take 2D_ARRAY_COMPATIBLE (VkResult %d) — its depthPlane attachments stay REFUSED",
              (unsigned)fmt, (unsigned)usage, (int)r);
    return r == VK_SUCCESS;
}
static void nvmtl_vk_image_mutable(VkImageCreateInfo *ici, nvk_image *out)
{
    ici->flags |= VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT;
    if (ici->imageType == VK_IMAGE_TYPE_2D && ici->samples == VK_SAMPLE_COUNT_1_BIT
        && nvmtl_vk_cube_compat(out->mtl_type, ici->extent.width, ici->extent.height, ici->arrayLayers))
        ici->flags |= VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT;
    if (ici->imageType == VK_IMAGE_TYPE_3D && (ici->usage & VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT)
        && nvmtl_vk_3d_slice_ok(ici->format, ici->usage))
        ici->flags |= VK_IMAGE_CREATE_2D_ARRAY_COMPATIBLE_BIT;
    out->vkflags = (uint32_t)ici->flags; out->usage = (uint32_t)ici->usage;
}
static int nvmtl_vk_view_guard(const nvk_image *img, uint32_t vkfmt, int a8, VkImageViewType vt, uint32_t levelCount,
                               VkImageViewUsageCreateInfo *uci, const char *who)
{
    const uint32_t t = img->mtl_type;
    int ok;
    switch (vt) {
        case VK_IMAGE_VIEW_TYPE_1D: case VK_IMAGE_VIEW_TYPE_1D_ARRAY: ok = t <= 1; break;
        case VK_IMAGE_VIEW_TYPE_2D: case VK_IMAGE_VIEW_TYPE_2D_ARRAY:
            ok = (t >= 2 && t <= 6) || t == 8
              || (t == 7 && (img->vkflags & VK_IMAGE_CREATE_2D_ARRAY_COMPATIBLE_BIT) && levelCount == 1); break;
        case VK_IMAGE_VIEW_TYPE_3D: ok = t == 7; break;
        case VK_IMAGE_VIEW_TYPE_CUBE: case VK_IMAGE_VIEW_TYPE_CUBE_ARRAY:
            ok = t == 5 || t == 6 || (img->vkflags & VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT) != 0; break;
        default: ok = 0; break;
    }
    if (!ok) { nvlog("%s: view type %d of a textureType-%u image refused (NVK asserts on it)", who, (int)vt, t); return -1; }
    if (vkfmt == img->fmt) return 0;
    if (!(img->vkflags & VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT)) {
        nvlog("%s: view format %u of a format-%u image that is not mutable (depth / buffer texture) refused", who, vkfmt, img->fmt); return -1; }
    if (img->vpair && vkfmt != img->vpair) {
        nvlog("%s: view format %u of a buffer texture (format %u) refused - its image lists only its sRGB/linear pair %u", who, vkfmt, img->fmt, img->vpair); return -1; }
    uint32_t bw = 1, bh = 1, bb = 0;
    if (img->bw > 1 && !nvmtl_vk_format_block(vkfmt, &bw, &bh, &bb) && levelCount != 1) {
        nvlog("%s: uncompressed view of a block-compressed image over %u levels refused (NVK allows 1)", who, levelCount); return -1; }
    if (!(img->usage & VK_IMAGE_USAGE_STORAGE_BIT) || (!a8 && nvmtl_vk_format_is_storage(vkfmt))) return 0;
    *uci = (VkImageViewUsageCreateInfo){ VK_STRUCTURE_TYPE_IMAGE_VIEW_USAGE_CREATE_INFO, NULL,
                                         (VkImageUsageFlags)(img->usage & ~(uint32_t)VK_IMAGE_USAGE_STORAGE_BIT) };
    return 1;
}
static uint32_t nvmtl_spirv_stex_hi(const void *p, size_t n)
{
    enum { STEX0 = 480, NSTEX = 128 };
    const uint32_t *w = (const uint32_t *)p; size_t nw = n / 4; uint32_t hi = 0;
    if (!p || nw < 5 || w[0] != 0x07230203u) return 0;
    for (size_t i = 5; i < nw; ) {
        uint32_t wc = w[i] >> 16, op = w[i] & 0xffffu;
        if (!wc || i + wc > nw) break;
        if (op == 71 && wc == 4 && w[i + 2] == 33) {
            uint32_t b = w[i + 3];
            if (b >= STEX0 && b < STEX0 + NSTEX && b - STEX0 + 1 > hi) hi = b - STEX0 + 1;
        }
        i += wc;
    }
    return hi;
}
int nvmtl_vk_image_create_mips(uint32_t w, uint32_t h, int bgra, uint32_t mips, nvk_image *out)
{
    return nvmtl_vk_image_create_ex(w, h, bgra ? VK_FORMAT_B8G8R8A8_UNORM : VK_FORMAT_R8G8B8A8_UNORM, 4, 0, mips, out);
}
int nvmtl_vk_image_create_ex(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, nvk_image *out)
{
    return nvmtl_vk_image_create_ex2(w, h, vkfmt, bpp, a8, mips, 0, out);
}
int nvmtl_vk_image_create_ex2(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, int storage, nvk_image *out)
{
    return nvmtl_vk_image_create_typed(w, h, vkfmt, bpp, a8, mips, storage, 2, 1, out);
}
uint64_t nvmtl_vk_vram_bytes(void)
{
    uint64_t best = 0;
    for (uint32_t i = 0; i < g_memp.memoryHeapCount; i++)
        if ((g_memp.memoryHeaps[i].flags & VK_MEMORY_HEAP_DEVICE_LOCAL_BIT) && g_memp.memoryHeaps[i].size > best) best = g_memp.memoryHeaps[i].size;
    return best;
}
void nvmtl_vk_mem_split(uint64_t *vram, uint64_t *sys) { if (vram) *vram = g_vram_bytes; if (sys) *sys = g_sys_bytes; }
static int nvmtl_vk_vram_budget(uint64_t *budget, uint64_t *used)
{
    static uint64_t cb, cu, when; static int measured;
    uint64_t now = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    if (!when || g_budget_stale || now - when > 50000000ull) {
        g_budget_stale = 0;
        uint32_t heap = 0; uint64_t best = 0;
        for (uint32_t i = 0; i < g_memp.memoryHeapCount; i++)
            if ((g_memp.memoryHeaps[i].flags & VK_MEMORY_HEAP_DEVICE_LOCAL_BIT) && g_memp.memoryHeaps[i].size > best) { best = g_memp.memoryHeaps[i].size; heap = i; }
        measured = 0; cb = best * 7 / 10; cu = g_vram_bytes;
        if (g_budget_ext && pvkGetPhysicalDeviceMemoryProperties2) {
            VkPhysicalDeviceMemoryBudgetPropertiesEXT b = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MEMORY_BUDGET_PROPERTIES_EXT, NULL, { 0 }, { 0 } };
            VkPhysicalDeviceMemoryProperties2 mp = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MEMORY_PROPERTIES_2, &b, { 0 } };
            pvkGetPhysicalDeviceMemoryProperties2(g_pdev, &mp);
            if (b.heapBudget[heap]) { cb = b.heapBudget[heap]; cu = b.heapUsage[heap]; measured = 1; }
        }
        when = now;
        g_budget_delta = 0;
    }
    *budget = cb; *used = cu; return measured;
}
static int nvmtl_vk_vram_wall0(uint64_t want, uint64_t *freev_out, uint64_t *bud_out, uint64_t *used_out, int *measured_out);
uint64_t (*nvmtl_reclaim_hook)(uint64_t need);
void (*nvmtl_res2_pressure_hook)(uint64_t want);
uint64_t (*nvmtl_res2_evict_hook)(uint64_t want);
static uint64_t g_res2_evict_calls, g_res2_evict_won;
static int nvmtl_res2_evict_off(void) {
    static int off = -1;
    if (off < 0) { const char *e = getenv("NVMTL_NO_RES2_EVICT"); off = (e && *e && *e != '0') ? 1 : 0; }
    return off;
}
static __thread int t_in_reclaim;
static uint64_t g_res1_calls, g_res1_credit, g_res1_seen_free, g_res1_admit;
static int nvmtl_vk_vram_wall(uint64_t want, uint64_t *freev_out, uint64_t *bud_out, uint64_t *used_out, int *measured_out)
{
    uint64_t fv = 0, bd = 0, us = 0; int ms = 0;
    int gate = nvmtl_vk_vram_wall0(want, &fv, &bd, &us, &ms);
    if (fv > g_res1_seen_free) { uint64_t up = fv - g_res1_seen_free; g_res1_credit = g_res1_credit > up ? g_res1_credit - up : 0; }
    g_res1_seen_free = fv;
    if (gate && nvmtl_reclaim_hook && !t_in_reclaim) {
        uint64_t need = want + NVMTL_VRAM_HEADROOM > fv + g_res1_credit ? want + NVMTL_VRAM_HEADROOM - fv - g_res1_credit : 0;
        if (need) {
            t_in_reclaim = 1; g_res1_calls++;
            g_res1_credit += nvmtl_reclaim_hook(need);
            t_in_reclaim = 0;
        }
        if (ms && want + NVMTL_VRAM_HEADROOM <= fv + g_res1_credit) {
            g_res1_credit = g_res1_credit > want ? g_res1_credit - want : 0; g_res1_admit++; gate = 0;
            { static unsigned said; if (said++ < 8 || said % 1000 == 0)
                nvlog("residency (res1): %llu MB admitted to VRAM on purged bytes RM has not shown yet (free %llu MB, credit left %llu MB, %llu admitted)",
                      (unsigned long long)(want >> 20), (unsigned long long)(fv >> 20), (unsigned long long)(g_res1_credit >> 20),
                      (unsigned long long)g_res1_admit); }
        }
    }
    if (gate && nvmtl_res2_pressure_hook && !t_in_reclaim) nvmtl_res2_pressure_hook(want);
    if (gate && nvmtl_res2_evict_hook && !t_in_reclaim && !nvmtl_res2_evict_off()) {
        t_in_reclaim = 1;
        const uint64_t freed = nvmtl_res2_evict_hook(want);
        t_in_reclaim = 0;
        if (freed) {
            g_budget_stale = 1;
            const int g2 = nvmtl_vk_vram_wall0(want, &fv, &bd, &us, &ms);
            if (!g2) { gate = 0; g_res2_evict_won++;
                { static unsigned said; if (said++ < 8 || said % 4096 == 0)
                    nvlog("residency (res2): evicted %llu MB on the allocating thread and ADMITTED a %llu MB "
                          "resource to VRAM (free now %llu MB; Apple's order - the newest wins the card; %llu admitted)",
                          (unsigned long long)(freed >> 20), (unsigned long long)(want >> 20),
                          (unsigned long long)(fv >> 20), (unsigned long long)g_res2_evict_won); } }
            g_res2_evict_calls++;
        }
    }
    if (freev_out) *freev_out = fv;
    if (bud_out) *bud_out = bd;
    if (used_out) *used_out = us;
    if (measured_out) *measured_out = ms;
    return gate;
}
static int nvmtl_vk_vram_wall0(uint64_t want, uint64_t *freev_out, uint64_t *bud_out, uint64_t *used_out, int *measured_out)
{
    uint64_t bud = 0, used = 0;
    int measured = nvmtl_vk_vram_budget(&bud, &used);
    uint64_t card = nvmtl_vk_vram_bytes();
    uint64_t inv = bud / 9 * 10;
    uint64_t freev = card;
    uint64_t head = NVMTL_VRAM_HEADROOM;
    int gate;
    if (!measured || !bud) gate = (used + g_budget_delta + want + head > bud);
    else if (inv >= card) gate = 0;
    else { freev = inv > used ? inv - used : 0;

           freev = freev > g_budget_delta ? freev - g_budget_delta : 0;
           gate = (want + head > freev); }
    if (freev_out) *freev_out = freev;
    if (bud_out) *bud_out = bud;
    if (used_out) *used_out = used;
    if (measured_out) *measured_out = measured;
    return gate;
}
uint64_t nvmtl_vk_working_set(void)
{
    uint64_t b = 0, u = 0; if (nvmtl_vk_init()) return 2ull << 30;
    { static int res2 = -1; if (res2 < 0) { const char *e = getenv("NVMTL_NO_RES2"), *w = getenv("NVMTL_RES2_WS");
          res2 = !(w && w[0] == '0') && g_sparse && !(e && e[0] == '1'); }
      if (res2 && nvmtl_vk_vram_bytes()) return nvmtl_vk_vram_bytes(); }
    nvmtl_vk_vram_budget(&b, &u);
    static long long ovr = -2; if (ovr == -2) { const char *e = getenv("NVMTL_VRAM_WS_NONIMAGE_MB"); ovr = (e && *e) ? atoll(e) : -1; }
    uint64_t nonimg = ovr >= 0 ? (uint64_t)ovr << 20 : NVMTL_VRAM_NONIMAGE;
    if (u > g_vram_bytes && u - g_vram_bytes > nonimg) nonimg = u - g_vram_bytes;
    uint64_t reserve = NVMTL_VRAM_HEADROOM + nonimg;
    uint64_t room = b > reserve ? b - reserve : 0;
    { static long long pct = -2; if (pct == -2) { const char *e = getenv("NVMTL_WS_FILL_PCT"); pct = (e && *e) ? atoll(e) : 92;
          if (pct < 50 || pct > 100) pct = 92; }
      room = room / 100 * (uint64_t)pct; }
    return room > (1ull << 30) ? room : (1ull << 30);
}
unsigned long long nvmtl_tex_objs_live(void);
unsigned long long nvmtl_tex_objs_released(void);
static void nvmtl_vk_image_census(void)
{
    if ((g_img_made & 0xFFF) == 0)
    { uint64_t bud = 0, used = 0; int m = nvmtl_vk_vram_budget(&bud, &used);
        nvlog("image accounting: VRAM images asked %llu MB, 64K-aligned %llu MB (+%llu waste); system images %llu -> %llu MB; all our allocations %llu MB (BAR1 %llu MB); NVK charges heap0 %llu MB -> NON-IMAGE %llu MB",
              (unsigned long long)(g_vram_bytes >> 20), (unsigned long long)(g_img_align >> 20),
              (unsigned long long)((g_img_align > g_vram_bytes ? g_img_align - g_vram_bytes : 0) >> 20),
              (unsigned long long)(g_sys_bytes >> 20), (unsigned long long)(g_sys_align >> 20),
              (unsigned long long)(((uint64_t)g_alloc_bytes) >> 20), (unsigned long long)(g_bar1_bytes >> 20),
              (unsigned long long)(used >> 20),
              (unsigned long long)((used > g_img_align ? used - g_img_align : 0) >> 20));
        nvlog("image census: %llu made, %llu gone, %llu live; VRAM %llu MB, system memory %llu MB (%llu spilled); card %llu MB; budget %llu MB used %llu MB (%s); MTLTexture objects %llu live, %llu released",
              (unsigned long long)g_img_made, (unsigned long long)g_img_gone, (unsigned long long)(g_img_made - g_img_gone),
              (unsigned long long)(g_vram_bytes >> 20), (unsigned long long)(g_sys_bytes >> 20), (unsigned long long)g_spilled,
              (unsigned long long)(nvmtl_vk_vram_bytes() >> 20), (unsigned long long)(bud >> 20), (unsigned long long)(used >> 20), m ? "measured" : "estimated",
              nvmtl_tex_objs_live(), nvmtl_tex_objs_released()); }
}
static VkResult nvmtl_vk_alloc_image_mem(const VkMemoryRequirements *mr, VkDeviceMemory *mem, uint32_t *sysmem, const char *what)
{
    *sysmem = 0;
    int mt = memtype(mr->memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    if (mt < 0) { nvlog("vk: no device-local memory type for the %s image", what); return VK_ERROR_OUT_OF_DEVICE_MEMORY; }
    VkMemoryAllocateInfo mai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, NULL, mr->size, (uint32_t)mt };
    uint64_t bud = 0, used = 0, freev = 0; int measured = 0;
    int gate = nvmtl_vk_vram_wall(mr->size, &freev, &bud, &used, &measured);
    VkResult r = VK_ERROR_OUT_OF_DEVICE_MEMORY;
    if (!gate) r = pvkAllocateMemory(g_dev, &mai, NULL, mem);
    else nvlog("vk: soft VRAM gate: %llu MB free on the card, this %s image wants %llu MB + %llu MB headroom -> SYSTEM MEMORY (heapUsage %llu of budget %llu MB, %s)",
               (unsigned long long)(freev >> 20), what, (unsigned long long)(mr->size >> 20),
               (unsigned long long)(NVMTL_VRAM_HEADROOM >> 20), (unsigned long long)(used >> 20),
               (unsigned long long)(bud >> 20), measured ? "measured" : "estimated");
    if (r == VK_ERROR_OUT_OF_DEVICE_MEMORY && nvmtl_res2_evict_hook && !t_in_reclaim && !nvmtl_res2_evict_off()) {
        t_in_reclaim = 1;
        const uint64_t freed = nvmtl_res2_evict_hook(mr->size | (1ull << 63));
        t_in_reclaim = 0;
        if (freed) {
            mai.memoryTypeIndex = (uint32_t)mt;
            r = pvkAllocateMemory(g_dev, &mai, NULL, mem);
            g_res2_evict_calls++; if (r == VK_SUCCESS) g_res2_evict_won++;
            { static unsigned said; if (said++ < 8 || said % 4096 == 0)
                nvlog("residency (res2): evicted %llu MB on the allocating thread for a %llu MB %s image -> %s "
                      "(%llu of %llu retries won the card)",
                      (unsigned long long)(freed >> 20), (unsigned long long)(mr->size >> 20), what,
                      r == VK_SUCCESS ? "VRAM" : "still system memory",
                      (unsigned long long)g_res2_evict_won, (unsigned long long)g_res2_evict_calls); }
        }
    }
    if (r == VK_ERROR_OUT_OF_DEVICE_MEMORY) {
        int mt2 = -1;
        for (uint32_t i = 0; i < g_memp.memoryTypeCount; i++)
            if ((mr->memoryTypeBits & (1u << i)) && !(g_memp.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) { mt2 = (int)i; break; }
        if (mt2 >= 0) { mai.memoryTypeIndex = (uint32_t)mt2; r = pvkAllocateMemory(g_dev, &mai, NULL, mem); if (r == VK_SUCCESS) *sysmem = 1; }
        if (r == VK_SUCCESS) { g_spilled++;
            if (nvmtl_purge_report_hook) nvmtl_purge_report_hook(what);
            nvlog("vk: %s image SPILLED to system memory (%s): images hold %llu MB VRAM + %llu MB system, heapUsage %llu MB, %llu MB free, %llu spilled so far",
                  what, gate ? "our soft gate" : "RM refused it",
                  (unsigned long long)(g_vram_bytes >> 20), (unsigned long long)(g_sys_bytes >> 20),
                  (unsigned long long)(used >> 20), (unsigned long long)(freev >> 20),
                  (unsigned long long)g_spilled); }
        else nvlog("vk: %s image: VRAM full and no system-memory type takes it (bits 0x%x) -> %d", what, mr->memoryTypeBits, r);
    }
    return r;
}
static VkResult nvmtl_alloc_opaque_image_mem(const VkMemoryRequirements *mr, void **out, uint32_t *sysmem, const char *what)
{
    VkDeviceMemory memory = VK_NULL_HANDLE;
    VkResult result = nvmtl_vk_alloc_image_mem(mr, &memory, sysmem, what);
    if (result == VK_SUCCESS) {
        *out = memory;
        if (!*sysmem) nvmtl_vk_budget_charge(mr->size);
    }
    return result;
}
static void nvmtl_failed_image(nvk_image *image)
{
    if (image->view) pvkDestroyImageView(g_dev, (VkImageView)image->view, NULL);
    if (image->img) pvkDestroyImage(g_dev, (VkImage)image->img, NULL);
    if (image->mem && !image->placed) {
        pvkFreeMemory(g_dev, (VkDeviceMemory)image->mem, NULL);

        if (!image->sysmem) g_budget_stale = 1;
    }
    memset(image, 0, sizeof *image);
}
static void nvmtl_commit_image(nvk_image *image)
{
    if (image->mem && !image->placed) {
        uint64_t aligned = (image->alloc + 65535ull) & ~65535ull;
        g_alloc_bytes += image->alloc;
        if (image->sysmem) { g_sys_bytes += image->alloc; g_sys_align += aligned; }
        else { g_vram_bytes += image->alloc; g_img_align += aligned; }
    }
    g_img_made++;
    nvmtl_vk_image_census();
}

static int nvmtl_vk_image_create_ms_lifetime_body(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, uint32_t samples, uint32_t mtl_type, uint32_t layers, nvk_image *out);
int nvmtl_vk_image_create_ms(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, uint32_t samples, nvk_image *out)
{
    return nvmtl_vk_image_create_ms_full(w, h, vkfmt, bpp, samples, 4, 1, out);
}
int nvmtl_vk_image_create_ms_full(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, uint32_t samples, uint32_t mtl_type, uint32_t layers, nvk_image *out)
{
    nvk_image pending = {0};
    int result = nvmtl_vk_image_create_ms_lifetime_body(w, h, vkfmt, bpp, samples, mtl_type, layers, &pending);
    if (result) { nvmtl_failed_image(&pending); memset(out, 0, sizeof *out); }
    else { nvmtl_commit_image(&pending); *out = pending; }
    return result;
}
static int nvmtl_vk_image_create_ms_lifetime_body(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, uint32_t samples, uint32_t mtl_type, uint32_t layers, nvk_image *out)
{
    if (!w || !h || !layers || (mtl_type != 4 && mtl_type != 8) || (mtl_type == 4 && layers != 1)) return -1;
    if (samples != 2 && samples != 4 && samples != 8) { nvlog("image_create: sampleCount %u is not carried (2, 4, 8)", samples); return -1; }
    if (nvmtl_vk_init()) return -1;
    memset(out, 0, sizeof *out); out->w = w; out->h = h; out->mips = 1; out->fmt = vkfmt; out->bpp = bpp ? bpp : 4; out->mtl_type = mtl_type; out->layers = layers; out->samples = samples;
    VkImageCreateInfo ici = { VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, NULL, 0, VK_IMAGE_TYPE_2D, (VkFormat)vkfmt, { w, h, 1 }, 1, layers,
        (VkSampleCountFlagBits)samples, VK_IMAGE_TILING_OPTIMAL, VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_SAMPLED_BIT,
        VK_SHARING_MODE_EXCLUSIVE, 0, NULL, VK_IMAGE_LAYOUT_UNDEFINED };
    nvmtl_vk_image_mutable(&ici, out);
    VKCK(nvmtl_opaque_CreateImage(g_dev, &ici, NULL, &out->img), "vkCreateImage(ms)");
    VkMemoryRequirements mr; pvkGetImageMemoryRequirements(g_dev, out->img, &mr);
    VKCK(nvmtl_alloc_opaque_image_mem(&mr, &out->mem, &out->sysmem, "multisample"), "vkAllocateMemory(ms)");
    out->alloc = mr.size;
    VKCK(pvkBindImageMemory(g_dev, out->img, out->mem, 0), "vkBindImageMemory(ms)");
    VkImageViewCreateInfo vci = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, out->img, mtl_type == 8 ? VK_IMAGE_VIEW_TYPE_2D_ARRAY : VK_IMAGE_VIEW_TYPE_2D, (VkFormat)vkfmt,
        { 0, 0, 0, 0 }, { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, layers } };
    VKCK(nvmtl_opaque_CreateImageView(g_dev, &vci, NULL, &out->view), "vkCreateImageView(ms)");
    nvlog("image_create: %ux%u %u-sample colour target", w, h, samples);
    return 0;
}
static int nvmtl_vk_image_create_typed_lifetime_body(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, int storage, uint32_t mtl_type, uint32_t layers, nvk_image *out);
int nvmtl_vk_image_create_typed(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, int storage, uint32_t mtl_type, uint32_t layers, nvk_image *out)
{
    nvk_image pending = {0};
    int result = nvmtl_vk_image_create_typed_lifetime_body(w, h, vkfmt, bpp, a8, mips, storage, mtl_type, layers, &pending);
    if (result) { nvmtl_failed_image(&pending); memset(out, 0, sizeof *out); }
    else { nvmtl_commit_image(&pending); *out = pending; }
    return result;
}
static int nvmtl_vk_image_create_typed_lifetime_body(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, int storage, uint32_t mtl_type, uint32_t layers, nvk_image *out)
{
    if (!layers || (mtl_type < 2 && h != 1) || mtl_type == 4 || mtl_type > 7
        || ((mtl_type == 0 || mtl_type == 2) && layers != 1) || (mtl_type == 5 && layers != 6) || (mtl_type == 6 && layers % 6)) {
        nvlog("image_create: textureType %u with %u layer(s) is not carried", mtl_type, layers); return -1; }
    if (nvmtl_vk_init()) return -1;
    if (!mips) mips = 1;
    VkFormat fmt = (VkFormat)vkfmt;
    storage = storage && !a8 && nvmtl_vk_format_is_storage(vkfmt);
    memset(out, 0, sizeof *out); out->w = w; out->h = h; out->mips = mips; out->fmt = vkfmt; out->bpp = bpp ? bpp : 4;
    out->storage = storage ? 1 : 0; out->mtl_type = mtl_type; out->layers = layers;
    { uint32_t bb = 0; if (nvmtl_vk_format_block(vkfmt, &out->bw, &out->bh, &bb)) { out->bpp = bb; out->storage = 0; storage = 0; } }
    VkImageCreateInfo ici = { VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, NULL, (mtl_type == 5 || mtl_type == 6) ? VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT : 0, mtl_type == 7 ? VK_IMAGE_TYPE_3D : mtl_type < 2 ? VK_IMAGE_TYPE_1D : VK_IMAGE_TYPE_2D,
        fmt, { w, h, mtl_type == 7 ? layers : 1 }, mips, mtl_type == 7 ? 1 : layers, VK_SAMPLE_COUNT_1_BIT, VK_IMAGE_TILING_OPTIMAL,
        (out->bw > 1 ? 0 : VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT) | VK_IMAGE_USAGE_TRANSFER_SRC_BIT |
        VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_SAMPLED_BIT | (storage ? VK_IMAGE_USAGE_STORAGE_BIT : 0),
        VK_SHARING_MODE_EXCLUSIVE, 0, NULL, VK_IMAGE_LAYOUT_UNDEFINED };
    nvmtl_vk_image_mutable(&ici, out);
    VKCK(nvmtl_opaque_CreateImage(g_dev, &ici, NULL, &out->img), "vkCreateImage");
    VkMemoryRequirements mr; pvkGetImageMemoryRequirements(g_dev, out->img, &mr);
    VKCK(nvmtl_alloc_opaque_image_mem(&mr, &out->mem, &out->sysmem, "colour"), "vkAllocateMemory(image)");
    out->alloc = mr.size;
    VKCK(pvkBindImageMemory(g_dev, out->img, out->mem, 0), "vkBindImageMemory");
    VkComponentMapping swz = a8 ? (VkComponentMapping){ VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_R }
                                : (VkComponentMapping){ 0, 0, 0, 0 };
    VkImageViewType viewType = mtl_type == 0 ? VK_IMAGE_VIEW_TYPE_1D : mtl_type == 1 ? VK_IMAGE_VIEW_TYPE_1D_ARRAY : mtl_type == 3 ? VK_IMAGE_VIEW_TYPE_2D_ARRAY
                             : mtl_type == 5 ? VK_IMAGE_VIEW_TYPE_CUBE : mtl_type == 6 ? VK_IMAGE_VIEW_TYPE_CUBE_ARRAY : mtl_type == 7 ? VK_IMAGE_VIEW_TYPE_3D : VK_IMAGE_VIEW_TYPE_2D;
    VkImageViewCreateInfo vci = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, out->img, viewType,
        fmt, swz, { VK_IMAGE_ASPECT_COLOR_BIT, 0, mips, 0, mtl_type == 7 ? 1 : layers } };
    VKCK(nvmtl_opaque_CreateImageView(g_dev, &vci, NULL, &out->view), "vkCreateImageView");

    return 0;
}
int nvmtl_vk_surface_share_on(void)
{
    static int on = -1;
    if (on < 0) {
        const char *e = getenv("NVMTL_NO_SURFACE_SHARE");
        on = pnvk_stage_import && g_extfd && g_linmod && !(e && e[0] == '1');
        nvlog("item 7: IOSurface textures %s (NVK import %s, VK_KHR_external_memory_fd %s, DRM linear %s, NVMTL_NO_SURFACE_SHARE=%s)",
              on ? "live in the surface's family VRAM (one per surface, every process)" : "private per process (copies)",
              pnvk_stage_import ? "present" : "ABSENT", g_extfd ? "present" : "ABSENT", g_linmod ? "present" : "ABSENT", e ?: "unset");
    }
    return on;
}
int nvmtl_vk_surface_dirty(uint32_t surfaceID, uint32_t plane)
{
    static int on = -1;
    if (on < 0) {
        const char *e = getenv("NVMTL_NO_SURFACE_PAGEOFF");
        on = pnvk_surface_dirty && !(e && e[0] == '1');
        nvlog("after a GPU write, IOSurface VRAM is %s (NVK dirty export %s, NVMTL_NO_SURFACE_PAGEOFF=%s)",
              on ? "marked newest; a CPU lock / the display pipe pages it off (Apple's page-off)" : "copied out eagerly (old)",
              pnvk_surface_dirty ? "present" : "ABSENT", e ?: "unset");
    }
    if (!on || !g_dev) return -1;
    uint32_t st = pnvk_surface_dirty(g_dev, surfaceID, plane);
    if (st) { static unsigned n; if (n++ < 8 || n % 2000 == 0) nvlog("surface %u dirty refused (%#x) - copy-out (%u so far)", surfaceID, st, n); }
    return st ? -1 : 0;
}
int nvmtl_vk_surface_vram(uint32_t surfaceID, uint32_t plane, size_t need, nvk_buffer *out)
{
    memset(out, 0, sizeof *out);
    if (!nvmtl_vk_surface_share_on() || !need) return -1;
    uint32_t all = g_memp.memoryTypeCount >= 32 ? 0xffffffffu : ((1u << g_memp.memoryTypeCount) - 1u);
    int mt = memtype(all, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    if (mt < 0) return -1;
    nvmtl_vram_rec r = { 3, surfaceID, plane, 0, (uint64_t)need };
    VkImportMemoryFdInfoKHR imp = { VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR, NULL, VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT,
                                    open("/dev/null", O_RDONLY | O_CLOEXEC) };
    if (imp.fd < 0) return -1;
    VkMemoryAllocateInfo mai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &imp, need, (uint32_t)mt };
    VkDeviceMemory m = VK_NULL_HANDLE;
    pnvk_stage_import(&r);
    VkResult vr = pvkAllocateMemory(g_dev, &mai, NULL, &m);
    pnvk_stage_import(NULL);
    if (vr != VK_SUCCESS) {
        close(imp.fd);
        static unsigned n; if (n++ < 8 || n % 1000 == 0)
            nvlog("item 7: surface %u plane %u: family VRAM import refused (%d) - private image + copies (%u so far)", surfaceID, plane, (int)vr, n);
        return -1;
    }
    VkBufferCreateInfo bci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, 0, need,
                               VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT, VK_SHARING_MODE_EXCLUSIVE, 0, NULL };
    VkBuffer b = VK_NULL_HANDLE;
    if (pvkCreateBuffer(g_dev, &bci, NULL, &b) != VK_SUCCESS || pvkBindBufferMemory(g_dev, b, m, 0) != VK_SUCCESS) {
        if (b) pvkDestroyBuffer(g_dev, b, NULL);
        pvkFreeMemory(g_dev, m, NULL);
        nvlog("item 7: surface %u: a buffer over the imported VRAM was refused - private", surfaceID);
        return -1;
    }
    out->buf = b; out->mem = m; out->size = need; out->alloc = 0; out->imported = 1;
    g_buf_made++;
    return 0;
}
int nvmtl_vk_image_create_fmt(uint32_t w, uint32_t h, int bgra, nvk_image *out) { return nvmtl_vk_image_create_mips(w, h, bgra, 1, out); }

int nvmtl_vk_image_create(uint32_t w, uint32_t h, nvk_image *out) { return nvmtl_vk_image_create_fmt(w, h, 0, out); }

#define NVMTL_NBUF   32
#define NVMTL_NTEX   128
#define NVMTL_NSAMP  32
#define NVMTL_ADDRESS_BINDING (NVMTL_NBUF + NVMTL_NTEX + NVMTL_NSAMP)
#define NVMTL_ADDRESS_BINDING_NUMBER 640
#define NVMTL_BINDING_NUMBER(i) ((i) == NVMTL_ADDRESS_BINDING ? NVMTL_ADDRESS_BINDING_NUMBER : \
                                 (i) >= NVMTL_STEX_BASE ? NVMTL_STEX_BINDING_NUMBER + ((i) - NVMTL_STEX_BASE) : (i))
#define NVMTL_SSBO_ALIGN 16
#define NVMTL_NSTEX 128
#define NVMTL_STEX_BASE (NVMTL_ADDRESS_BINDING + 1)
#define NVMTL_STEX_BINDING_NUMBER 480
#define NVMTL_NBIND (NVMTL_STEX_BASE + NVMTL_NSTEX)
#define NVMTL_DISPATCH_PC_BYTES 48
_Static_assert(NVMTL_STEX_BINDING_NUMBER == 480 && NVMTL_NSTEX == 128, "nvmtl_spirv_stex_hi() scans for this band by value");

#define NVMTL_MAX_SETS 4096
int nvmtl_vk_dual_src(void) { return g_dual; }
int nvmtl_spirv_has_index1(const void *p, size_t n)
{
    const uint32_t *w = (const uint32_t *)p; size_t nw = n / 4;
    if (!p || nw < 5 || w[0] != 0x07230203u) return 0;
    for (size_t i = 5; i < nw; ) {
        uint32_t wc = w[i] >> 16, op = w[i] & 0xffffu;
        if (!wc || i + wc > nw) return 0;
        if (op == 71 && wc == 4 && w[i + 2] == 32 && w[i + 3] == 1) return 1;
        i += wc;
    }
    return 0;
}
static int nvmtl_spirv_bands_ok(const void *p, size_t n, const char *what)
{
    const uint32_t *w = (const uint32_t *)p; size_t nw = n / 4;
    if (!p || nw < 5 || w[0] != 0x07230203u) return 1;
    const uint32_t bound = w[3];
    uint8_t *heap = bound && bound < (1u << 24) ? (uint8_t *)calloc(bound, 1) : NULL;
    for (size_t i = 5; heap && i < nw; ) {
        uint32_t wc = w[i] >> 16, op = w[i] & 0xffffu;
        if (!wc || i + wc > nw) break;
        if (op == 71 && wc == 4 && w[i + 2] == 34 && w[i + 3] == 2 && w[i + 1] < bound) heap[w[i + 1]] = 1;
        i += wc;
    }
    int ok = 1;
    for (size_t i = 5; i < nw; ) {
        uint32_t wc = w[i] >> 16, op = w[i] & 0xffffu;
        if (!wc || i + wc > nw) break;
        if (op == 71 && wc == 4 && w[i + 2] == 33) {
            uint32_t id = w[i + 1], b = w[i + 3];
            int inheap = heap && id < bound && heap[id];
            if (!inheap && !(b < NVMTL_ADDRESS_BINDING || b == NVMTL_ADDRESS_BINDING_NUMBER ||
                             (b >= NVMTL_STEX_BINDING_NUMBER && b < NVMTL_STEX_BINDING_NUMBER + NVMTL_NSTEX))) {
                const char *band = b >= 192 && b < 200 ? "[[color(n)]] framebuffer fetch (programmable blending)"
                                 : b >= 200 && b < 480 ? "imageblock / tile memory" : "no band this driver builds";
                static _Atomic int said;
                if (said++ < 8)
                    nvlog("vk: %s declares descriptor binding %u — %s, a band this device's set layout does not have; built "
                          "(a read that survives constant folding still stops in NVK's descriptor lowering)", what, b, band);
                break;
            }
        }
        i += wc;
    }
    free(heap);
    return ok;
}
static int nvmtl_spirv_bmask(const void *p, size_t n, uint64_t out[4])
{
    const uint32_t *w = (const uint32_t *)p; size_t nw = n / 4;
    memset(out, 0, 4 * sizeof *out);
    if (!p || nw < 5 || w[0] != 0x07230203u) return 0;
    for (size_t i = 5; i < nw; ) {
        uint32_t wc = w[i] >> 16, op = w[i] & 0xffffu;
        if (!wc || i + wc > nw) return 0;
        if (op == 71 && wc == 4 && w[i + 2] == 33) {
            uint32_t b = w[i + 3];
            if (b == NVMTL_ADDRESS_BINDING_NUMBER) out[NVMTL_ADDRESS_BINDING >> 6] |= 1ull << (NVMTL_ADDRESS_BINDING & 63);
            else if (b < NVMTL_ADDRESS_BINDING) out[b >> 6] |= 1ull << (b & 63);
            else if (!(b >= NVMTL_STEX_BINDING_NUMBER && b < NVMTL_STEX_BINDING_NUMBER + NVMTL_NSTEX)) return 0;
        }
        i += wc;
    }
    return 1;
}
enum { NVMTL_SLOT_NONE = 0, NVMTL_SLOT_BUF, NVMTL_SLOT_TEX, NVMTL_SLOT_SAMP, NVMTL_SLOT_TEXBUF  };
typedef struct { uint8_t kind; void *h; size_t off; size_t size; uint64_t address; } nvmtl_slot;
#define NVMTL_EMB_KEYMAX 480
static int nvmtl_dirty_only_on(void)
{
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_NO_DIRTYONLY"); on = !(e && *e && *e != '0');
        nvlog("vk: dirty-only bindings %s (NVMTL_NO_DIRTYONLY=%s)", on ? "ON - unchanged descriptors are copied, unchanged argument-buffer tables skipped" : "OFF", e ? e : "unset"); }
    return on;
}
typedef struct { nvmtl_slot s[2][NVMTL_NBIND];
                 uint8_t dirty[2]; VkDescriptorSet last[2]; unsigned last_idx[2]; uint32_t last_hi[2]; uint64_t lastw[2][4];
                 uint64_t chg[2][4]; uint64_t texgen[2]; const void *emb_fn[2]; uint64_t emb_gen[2]; uint32_t emb_n[2];
                 uint64_t emb_key[2][NVMTL_EMB_KEYMAX]; } nvmtl_bindtable;

static VkDescriptorSetLayout g_setLayout;
#define NVMTL_MAX_POOLS 64
static VkDescriptorPool      g_descPools[NVMTL_MAX_POOLS];
static unsigned              g_ndescPools;
static pthread_mutex_t       g_desc_lock = PTHREAD_MUTEX_INITIALIZER;
static unsigned              g_peak_sets, g_peak_fbs;
static nvk_buffer            g_dummyBuf;
static nvk_image             g_dummyImg;
#define NVMTL_BINDLESS_SLOTS      131072
#define NVMTL_BINDLESS_QUARANTINE 1024
static VkDescriptorSetLayout g_heapLayout; static VkDescriptorPool g_heapPool; static VkDescriptorSet g_heapSet;
static pthread_mutex_t g_heap_lock = PTHREAD_MUTEX_INITIALIZER;
static uint32_t g_heap_next = 1;
static uint32_t g_heap_fifo[NVMTL_BINDLESS_SLOTS]; static uint32_t g_heap_fifo_head, g_heap_fifo_n, g_heap_live;

static nvk_image             g_dummyStex;
static VkSampler             g_dummySampler;

#define NVMTL_SAMPLER_INTERN_CAP 4096u
#define NVMTL_SAMPLER_INTERN_BUCKETS 8192u
typedef struct { uint32_t words[16]; } nvmtl_sampler_key;
typedef struct {
    nvmtl_sampler_key key;
    VkSampler handle;
    uint64_t refs;
    uint32_t state_next, handle_next;
} nvmtl_sampler_intern_entry;
static nvmtl_sampler_intern_entry g_sampler_entries[NVMTL_SAMPLER_INTERN_CAP + 1];
static uint32_t g_sampler_state_heads[NVMTL_SAMPLER_INTERN_BUCKETS];
static uint32_t g_sampler_handle_heads[NVMTL_SAMPLER_INTERN_BUCKETS];
static uint32_t g_sampler_intern_next = 1, g_sampler_intern_free, g_sampler_intern_live;
static pthread_mutex_t g_sampler_intern_lock = PTHREAD_MUTEX_INITIALIZER;

static uint32_t nvmtl_sampler_float_bits(float value)
{
    uint32_t bits;
    memcpy(&bits, &value, sizeof(bits));
    return bits;
}

static int nvmtl_sampler_make_key(const VkSamplerCreateInfo *s, nvmtl_sampler_key *key)
{

    if (!s || s->sType != VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO || s->pNext)
        return -1;
    *key = (nvmtl_sampler_key){{
        s->flags, s->magFilter, s->minFilter, s->mipmapMode,
        s->addressModeU, s->addressModeV, s->addressModeW,
        nvmtl_sampler_float_bits(s->mipLodBias), s->anisotropyEnable,
        nvmtl_sampler_float_bits(s->maxAnisotropy), s->compareEnable,
        s->compareOp, nvmtl_sampler_float_bits(s->minLod),
        nvmtl_sampler_float_bits(s->maxLod), s->borderColor,
        s->unnormalizedCoordinates
    }};
    return 0;
}

static uint32_t nvmtl_sampler_key_bucket(const nvmtl_sampler_key *key)
{
    uint32_t hash = 2166136261u;
    for (size_t i = 0; i < sizeof(key->words) / sizeof(key->words[0]); ++i)
        hash = (hash ^ key->words[i]) * 16777619u;
    hash ^= hash >> 16;
    return hash & (NVMTL_SAMPLER_INTERN_BUCKETS - 1);
}

static uint32_t nvmtl_sampler_handle_bucket(VkSampler handle)
{
    uint64_t bits = (uint64_t)(uintptr_t)handle;
    bits ^= bits >> 33;
    bits *= UINT64_C(0xff51afd7ed558ccd);
    bits ^= bits >> 33;
    return (uint32_t)bits & (NVMTL_SAMPLER_INTERN_BUCKETS - 1);
}

static VkResult nvmtl_sampler_acquire(const VkSamplerCreateInfo *info, VkSampler *out)
{
    if (!out) return VK_ERROR_INITIALIZATION_FAILED;
    *out = VK_NULL_HANDLE;
    nvmtl_sampler_key key;
    if (nvmtl_sampler_make_key(info, &key)) return VK_ERROR_FEATURE_NOT_PRESENT;
    const uint32_t state_bucket = nvmtl_sampler_key_bucket(&key);
    pthread_mutex_lock(&g_sampler_intern_lock);
    for (uint32_t i = g_sampler_state_heads[state_bucket]; i; i = g_sampler_entries[i].state_next) {
        nvmtl_sampler_intern_entry *entry = &g_sampler_entries[i];
        if (memcmp(&entry->key, &key, sizeof(key)) != 0) continue;
        if (entry->refs == UINT64_MAX) {
            pthread_mutex_unlock(&g_sampler_intern_lock);
            return VK_ERROR_TOO_MANY_OBJECTS;
        }
        ++entry->refs;
        *out = entry->handle;
        pthread_mutex_unlock(&g_sampler_intern_lock);
        return VK_SUCCESS;
    }
    uint32_t capacity = g_lim.maxSamplerAllocationCount;
    if (capacity > NVMTL_SAMPLER_INTERN_CAP) capacity = NVMTL_SAMPLER_INTERN_CAP;
    if (g_sampler_intern_live >= capacity) {
        pthread_mutex_unlock(&g_sampler_intern_lock);
        return VK_ERROR_TOO_MANY_OBJECTS;
    }
    VkSampler handle = VK_NULL_HANDLE;
    VkResult result = pvkCreateSampler(g_dev, info, NULL, &handle);
    if (result != VK_SUCCESS || handle == VK_NULL_HANDLE) {
        pthread_mutex_unlock(&g_sampler_intern_lock);
        return result == VK_SUCCESS ? VK_ERROR_INITIALIZATION_FAILED : result;
    }
    uint32_t index;
    if (g_sampler_intern_free) {
        index = g_sampler_intern_free;
        g_sampler_intern_free = g_sampler_entries[index].state_next;
    } else {
        index = g_sampler_intern_next++;
    }
    uint32_t handle_bucket = nvmtl_sampler_handle_bucket(handle);
    g_sampler_entries[index] = (nvmtl_sampler_intern_entry){
        .key = key, .handle = handle, .refs = 1,
        .state_next = g_sampler_state_heads[state_bucket],
        .handle_next = g_sampler_handle_heads[handle_bucket]
    };
    g_sampler_state_heads[state_bucket] = index;
    g_sampler_handle_heads[handle_bucket] = index;
    ++g_sampler_intern_live;
    *out = handle;
    pthread_mutex_unlock(&g_sampler_intern_lock);
    return VK_SUCCESS;
}

static void nvmtl_sampler_release(VkSampler handle)
{
    if (handle == VK_NULL_HANDLE) return;
    pthread_mutex_lock(&g_sampler_intern_lock);
    uint32_t *handle_link = &g_sampler_handle_heads[nvmtl_sampler_handle_bucket(handle)];
    while (*handle_link && g_sampler_entries[*handle_link].handle != handle)
        handle_link = &g_sampler_entries[*handle_link].handle_next;
    if (!*handle_link) {
        pthread_mutex_unlock(&g_sampler_intern_lock);
        nvlog("sampler release: handle is not owned by the sampler interner");
        return;
    }
    uint32_t index = *handle_link;
    nvmtl_sampler_intern_entry *entry = &g_sampler_entries[index];
    if (--entry->refs == 0) {
        *handle_link = entry->handle_next;
        uint32_t *state_link = &g_sampler_state_heads[nvmtl_sampler_key_bucket(&entry->key)];
        while (*state_link != index)
            state_link = &g_sampler_entries[*state_link].state_next;
        *state_link = entry->state_next;

        pvkDestroySampler(g_dev, handle, NULL);
        *entry = (nvmtl_sampler_intern_entry){ .state_next = g_sampler_intern_free };
        g_sampler_intern_free = index;
        --g_sampler_intern_live;
    }
    pthread_mutex_unlock(&g_sampler_intern_lock);
}

static int nvmtl_table_ubo(void)
{
    static int v = -1;
    if (v < 0) {
        (void)nvmtl_xlate_ready();
        const char *e = getenv("NVMTL_TABLE_SSBO");
        v = g_scalar_layout && xlate_lower_ex && !(e && e[0] == '1');
        nvlog("G1: address table (binding 640) is %s (NVMTL_TABLE_SSBO=%s, scalarBlockLayout %d, translator lower_ex %s)",
              v ? "a UNIFORM_BUFFER_DYNAMIC - an NVK constant bank, read with ldc" : "a STORAGE buffer (as before)", e ? e : "unset",
              g_scalar_layout, xlate_lower_ex ? "yes" : "NO");
    }
    return v;
}
static const uint32_t g_dyn_zero[2] = { 0, 0 };
static VkDescriptorType binding_type(uint32_t i)
{
    if (i == NVMTL_ADDRESS_BINDING && nvmtl_table_ubo()) return VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC;
    if (i >= NVMTL_STEX_BASE) return VK_DESCRIPTOR_TYPE_STORAGE_IMAGE;
    if (i < NVMTL_NBUF || i == NVMTL_ADDRESS_BINDING) return VK_DESCRIPTOR_TYPE_STORAGE_BUFFER;
    if (i < NVMTL_NBUF + NVMTL_NTEX) return VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE;
    return VK_DESCRIPTOR_TYPE_SAMPLER;
}

static int nvmtl_desc_pool_open(void)
{
    if (g_ndescPools == NVMTL_MAX_POOLS) {
        nvlog("vk: descriptor pool CEILING: %u pools x %d sets all live in one frame — draws are being skipped", g_ndescPools, NVMTL_MAX_SETS);
        return -1;
    }
    VkDescriptorPoolSize sizes[5] = {
        { VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC, NVMTL_MAX_SETS },
        { VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, (NVMTL_NBUF + 1) * NVMTL_MAX_SETS },
        { VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,  NVMTL_NTEX  * NVMTL_MAX_SETS },
        { VK_DESCRIPTOR_TYPE_SAMPLER,        NVMTL_NSAMP * NVMTL_MAX_SETS },
        { VK_DESCRIPTOR_TYPE_STORAGE_IMAGE,  NVMTL_NSTEX * NVMTL_MAX_SETS } };
    VkDescriptorPoolCreateInfo pci = { VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, NULL,
        VK_DESCRIPTOR_POOL_CREATE_FREE_DESCRIPTOR_SET_BIT, NVMTL_MAX_SETS, 5, sizes };
    static const VkDescriptorType mut4[4] = { VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE, VK_DESCRIPTOR_TYPE_UNIFORM_TEXEL_BUFFER,
                                              VK_DESCRIPTOR_TYPE_STORAGE_IMAGE, VK_DESCRIPTOR_TYPE_STORAGE_TEXEL_BUFFER };
    VkMutableDescriptorTypeListEXT plists[5] = { { 0, NULL }, { 0, NULL }, { 4, mut4 }, { 0, NULL }, { 4, mut4 } };
    VkMutableDescriptorTypeCreateInfoEXT pmi = { VK_STRUCTURE_TYPE_MUTABLE_DESCRIPTOR_TYPE_CREATE_INFO_EXT, NULL, 5, plists };
    if (g_mutable_layout) { sizes[2].type = VK_DESCRIPTOR_TYPE_MUTABLE_EXT; sizes[4].type = VK_DESCRIPTOR_TYPE_MUTABLE_EXT; pci.pNext = &pmi; }
    VKCK(pvkCreateDescriptorPool(g_dev, &pci, NULL, &g_descPools[g_ndescPools]), "vkCreateDescriptorPool");
    g_ndescPools++;
    if (g_ndescPools > 1)
        nvlog("vk: descriptor pool %u opened — one frame needed more than %u sets (%u draws)",
              g_ndescPools, (g_ndescPools - 1) * NVMTL_MAX_SETS, (g_ndescPools - 1) * NVMTL_MAX_SETS / 2);
    return 0;
}

static int g_descriptor_state;
static void nvmtl_vk_zero_dummies(void);
static int ensure_descriptors_locked(void)
{
    if (g_descriptor_state) return g_descriptor_state == 1 ? 0 : -1;
    g_descriptor_state = -1;
    VkDescriptorSetLayoutBinding b[NVMTL_NBIND];
    for (uint32_t i = 0; i < NVMTL_NBIND; i++) {
        b[i].binding = NVMTL_BINDING_NUMBER(i); b[i].descriptorType = binding_type(i); b[i].descriptorCount = 1;
        b[i].stageFlags = VK_SHADER_STAGE_ALL; b[i].pImmutableSamplers = NULL;
    }
    VkDescriptorSetLayoutCreateInfo lci = { VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, NULL, 0, NVMTL_NBIND, b };
    static const VkDescriptorType texTypes[2] = { VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE, VK_DESCRIPTOR_TYPE_UNIFORM_TEXEL_BUFFER };
    static const VkDescriptorType stexTypes[2] = { VK_DESCRIPTOR_TYPE_STORAGE_IMAGE, VK_DESCRIPTOR_TYPE_STORAGE_TEXEL_BUFFER };
    static VkMutableDescriptorTypeListEXT mlists[NVMTL_NBIND];
    VkMutableDescriptorTypeCreateInfoEXT mci = { VK_STRUCTURE_TYPE_MUTABLE_DESCRIPTOR_TYPE_CREATE_INFO_EXT, NULL, NVMTL_NBIND, mlists };
    g_mutable_layout = g_mutable;
    if (g_mutable_layout) {
        for (uint32_t i = 0; i < NVMTL_NBIND; i++) {
            mlists[i] = (VkMutableDescriptorTypeListEXT){ 0, NULL };
            if (b[i].descriptorType == VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE) { b[i].descriptorType = VK_DESCRIPTOR_TYPE_MUTABLE_EXT; mlists[i] = (VkMutableDescriptorTypeListEXT){ 2, texTypes }; }
            else if (b[i].descriptorType == VK_DESCRIPTOR_TYPE_STORAGE_IMAGE) { b[i].descriptorType = VK_DESCRIPTOR_TYPE_MUTABLE_EXT; mlists[i] = (VkMutableDescriptorTypeListEXT){ 2, stexTypes }; }
        }
        lci.pNext = &mci;
    }
    VKCK(pvkCreateDescriptorSetLayout(g_dev, &lci, NULL, &g_setLayout), "vkCreateDescriptorSetLayout");
    if (nvmtl_desc_pool_open()) return -1;
    if (nvmtl_vk_buffer_create(256, 1, &g_dummyBuf)) return -1;
    if (nvmtl_vk_image_create_fmt(1, 1, 0, &g_dummyImg)) return -1;
    if (nvmtl_vk_image_create_ex2(1, 1, VK_FORMAT_R8G8B8A8_UNORM, 4, 0, 1, 1, &g_dummyStex) || !g_dummyStex.storage) {
        nvlog("vk: NO storage dummy image (RGBA8 storage refused) — a declared-but-unbound storage texture stays unwritten");
        g_dummyStex.view = NULL;
    }
    if (g_mutable && nvmtl_vk_texel_view_create(&g_dummyBuf, 0, 256, VK_FORMAT_R8G8B8A8_UNORM, 1, &g_dummyTexAlias, &g_dummyTexView, NULL)) {
        nvlog("vk: par3 NO texel dummy — texture buffers are turned OFF for this process (the layout stays mutable)"); g_mutable = 0; }
    VkSamplerCreateInfo sci = { VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO };
    sci.magFilter = sci.minFilter = VK_FILTER_LINEAR; sci.maxLod = VK_LOD_CLAMP_NONE;
    VKCK(nvmtl_sampler_acquire(&sci, &g_dummySampler), "sampler acquire (dummy)");
    nvmtl_vk_zero_dummies();
    if (g_bindless) {
        VkDescriptorBindingFlags hf = VK_DESCRIPTOR_BINDING_PARTIALLY_BOUND_BIT | VK_DESCRIPTOR_BINDING_UPDATE_AFTER_BIND_BIT |
                                      VK_DESCRIPTOR_BINDING_UPDATE_UNUSED_WHILE_PENDING_BIT;
        VkDescriptorSetLayoutBindingFlagsCreateInfo hfi = { VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_BINDING_FLAGS_CREATE_INFO, NULL, 1, &hf };
        VkDescriptorSetLayoutBinding hb = { 0, VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE, NVMTL_BINDLESS_SLOTS, VK_SHADER_STAGE_ALL, NULL };
        VkDescriptorSetLayoutCreateInfo hli = { VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, &hfi,
                                                VK_DESCRIPTOR_SET_LAYOUT_CREATE_UPDATE_AFTER_BIND_POOL_BIT, 1, &hb };
        VkDescriptorPoolSize hs = { VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE, NVMTL_BINDLESS_SLOTS };
        VkDescriptorPoolCreateInfo hpi = { VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, NULL,
                                           VK_DESCRIPTOR_POOL_CREATE_UPDATE_AFTER_BIND_BIT, 1, 1, &hs };
        VkDescriptorSetLayout hl = VK_NULL_HANDLE; VkDescriptorPool hp = VK_NULL_HANDLE; VkDescriptorSet hset = VK_NULL_HANDLE;
        VkResult hr = pvkCreateDescriptorSetLayout(g_dev, &hli, NULL, &hl);
        if (hr == VK_SUCCESS) hr = pvkCreateDescriptorPool(g_dev, &hpi, NULL, &hp);
        if (hr == VK_SUCCESS) { VkDescriptorSetAllocateInfo hai = { VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, NULL, hp, 1, &hl };
                                hr = pvkAllocateDescriptorSets(g_dev, &hai, &hset); }
        VkDescriptorImageInfo *fill = hr == VK_SUCCESS ? (VkDescriptorImageInfo *)malloc(sizeof *fill * NVMTL_BINDLESS_SLOTS) : NULL;
        if (fill && g_dummyImg.view) {
            for (uint32_t k = 0; k < NVMTL_BINDLESS_SLOTS; k++)
                fill[k] = (VkDescriptorImageInfo){ VK_NULL_HANDLE, (VkImageView)g_dummyImg.view, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL };
            VkWriteDescriptorSet hw = { VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, NULL, hset, 0, 0, NVMTL_BINDLESS_SLOTS,
                                        VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE, fill, NULL, NULL };
            pvkUpdateDescriptorSets(g_dev, 1, &hw, 0, NULL);
            g_heapLayout = hl; g_heapPool = hp; g_heapSet = hset;
            nvlog("vk: BINDLESS heap ready - set 2, %d sampled images, every slot the dummy until a texture asks for its gpuResourceID", NVMTL_BINDLESS_SLOTS);
        } else {
            nvlog("vk: BINDLESS heap NOT created (vk result %d, fill %s) - gpuResourceID stays a lookup token, a runtime-indexed texture reads element 0",
                  (int)hr, fill ? "ok" : "none");
        }
        free(fill);
    }

    nvlog("vk: descriptor model ready — set 0, %d bindings (buffers 0-%d, textures %d-%d, samplers %d-%d)",
          NVMTL_NBIND, NVMTL_NBUF - 1, NVMTL_NBUF, NVMTL_NBUF + NVMTL_NTEX - 1, NVMTL_NBUF + NVMTL_NTEX, NVMTL_ADDRESS_BINDING - 1);
    nvlog("vk: + address table at %d, storage textures %d-%d (dummy %s)", NVMTL_ADDRESS_BINDING_NUMBER,
          NVMTL_STEX_BINDING_NUMBER, NVMTL_STEX_BINDING_NUMBER + NVMTL_NSTEX - 1, g_dummyStex.view ? "ok" : "MISSING");
    g_descriptor_state = 1;
    return 0;
}

static int ensure_descriptors(void) {
    pthread_mutex_lock(&g_desc_lock);
    int result = ensure_descriptors_locked();
    pthread_mutex_unlock(&g_desc_lock);
    return result;
}

static void nvmtl_heap_write_locked(uint32_t slot, void *view)
{
    VkDescriptorImageInfo ii = { VK_NULL_HANDLE, (VkImageView)view, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL };
    VkWriteDescriptorSet w = { VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, NULL, g_heapSet, 0, slot, 1, VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE, &ii, NULL, NULL };
    pvkUpdateDescriptorSets(g_dev, 1, &w, 0, NULL);
}
uint32_t nvmtl_vk_bindless_put(void *view)
{
    if (!view || ensure_descriptors() || !g_heapSet) return 0;
    uint32_t slot = 0, live = 0;
    pthread_mutex_lock(&g_heap_lock);
    if (g_heap_next < NVMTL_BINDLESS_SLOTS) slot = g_heap_next++;
    else if (g_heap_fifo_n > NVMTL_BINDLESS_QUARANTINE) {
        slot = g_heap_fifo[g_heap_fifo_head]; g_heap_fifo_head = (g_heap_fifo_head + 1) % NVMTL_BINDLESS_SLOTS; g_heap_fifo_n--;
    }
    if (slot) { nvmtl_heap_write_locked(slot, view); live = ++g_heap_live; }
    pthread_mutex_unlock(&g_heap_lock);
    if (!slot) { static int said; if (!said++) nvlog("vk: BINDLESS heap FULL (%d live) - this texture gets a lookup token, not a slot", NVMTL_BINDLESS_SLOTS); }
    else if ((live & 0xFFF) == 0) nvlog("vk: BINDLESS heap census - %u textures hold slots", live);
    return slot;
}
void nvmtl_vk_bindless_drop(uint32_t slot);
#define NVMTL_BL_CAP (1u << 18)
static void **g_bl_key; static uint32_t *g_bl_val; static uint32_t g_bl_n; static pthread_mutex_t g_bl_lock = PTHREAD_MUTEX_INITIALIZER;
static uint32_t nvmtl_bl_hash(const void *p) { uint64_t x = (uint64_t)(uintptr_t)p; x ^= x >> 33; x *= 0xff51afd7ed558ccdull; x ^= x >> 33; return (uint32_t)x & (NVMTL_BL_CAP - 1); }
static int nvmtl_bl_find_locked(const void *v) { uint32_t h = nvmtl_bl_hash(v); while (g_bl_key[h]) { if (g_bl_key[h] == v) return (int)h; h = (h + 1) & (NVMTL_BL_CAP - 1); } return -1; }
uint32_t nvmtl_bl_slot(void *view)
{
    if (!view || !g_heapSet) return 0;
    pthread_mutex_lock(&g_bl_lock);
    if (!g_bl_key) { g_bl_key = (void **)calloc(NVMTL_BL_CAP, sizeof *g_bl_key); g_bl_val = (uint32_t *)calloc(NVMTL_BL_CAP, sizeof *g_bl_val);
        if (!g_bl_key || !g_bl_val) { free(g_bl_key); free(g_bl_val); g_bl_key = NULL; g_bl_val = NULL; pthread_mutex_unlock(&g_bl_lock); return 0; } }
    int at = nvmtl_bl_find_locked(view);
    if (at >= 0) { uint32_t s = g_bl_val[at]; pthread_mutex_unlock(&g_bl_lock); return s; }
    const int full = g_bl_n >= NVMTL_BL_CAP / 2;
    pthread_mutex_unlock(&g_bl_lock);
    if (full) { static int said; if (!said++) nvlog("vk: bindless-all map FULL - this texture reads the dummy"); return 0; }
    uint32_t s = nvmtl_vk_bindless_put(view);
    if (!s) return 0;
    pthread_mutex_lock(&g_bl_lock);
    at = nvmtl_bl_find_locked(view);
    if (at >= 0) { uint32_t won = g_bl_val[at]; pthread_mutex_unlock(&g_bl_lock); nvmtl_vk_bindless_drop(s); return won; }
    uint32_t h = nvmtl_bl_hash(view); while (g_bl_key[h]) h = (h + 1) & (NVMTL_BL_CAP - 1);
    g_bl_key[h] = view; g_bl_val[h] = s; g_bl_n++;
    pthread_mutex_unlock(&g_bl_lock);
    { static unsigned said; if (said++ < 2) nvlog("vk: bindless-all - textures reach shaders as heap slots in the per-draw table (first slot %u)", s); }
    return s;
}
static void nvmtl_bl_forget(const void *view)
{
    if (!view) return;
    pthread_mutex_lock(&g_bl_lock);
    int at = g_bl_key ? nvmtl_bl_find_locked(view) : -1;
    if (at < 0) { pthread_mutex_unlock(&g_bl_lock); return; }
    uint32_t slot = g_bl_val[at], i = (uint32_t)at, j = i;
    g_bl_key[i] = NULL; g_bl_n--;
    for (;;) {
        j = (j + 1) & (NVMTL_BL_CAP - 1);
        if (!g_bl_key[j]) break;
        uint32_t k = nvmtl_bl_hash(g_bl_key[j]);
        if ((j > i && (k <= i || k > j)) || (j < i && (k <= i && k > j))) { g_bl_key[i] = g_bl_key[j]; g_bl_val[i] = g_bl_val[j]; g_bl_key[j] = NULL; i = j; }
    }
    pthread_mutex_unlock(&g_bl_lock);
    nvmtl_vk_bindless_drop(slot);
}
static PFN_vkDestroyImageView g_bl_real_destroy_view;
static VKAPI_ATTR void VKAPI_CALL nvmtl_bl_destroy_view(VkDevice d, VkImageView v, const VkAllocationCallbacks *a)
{ nvmtl_bl_forget((const void *)v); g_bl_real_destroy_view(d, v, a); }
void nvmtl_bl_hook_destroy(void) { if (!g_bl_real_destroy_view && pvkDestroyImageView) { g_bl_real_destroy_view = pvkDestroyImageView; pvkDestroyImageView = nvmtl_bl_destroy_view; } }
void nvmtl_vk_bindless_drop(uint32_t slot)
{
    if (!g_heapSet || !slot || slot >= NVMTL_BINDLESS_SLOTS) return;
    pthread_mutex_lock(&g_heap_lock);
    nvmtl_heap_write_locked(slot, g_dummyImg.view);
    g_heap_fifo[(g_heap_fifo_head + g_heap_fifo_n) % NVMTL_BINDLESS_SLOTS] = slot; g_heap_fifo_n++;
    if (g_heap_live) g_heap_live--;
    pthread_mutex_unlock(&g_heap_lock);
}

int nvmtl_vk_bind_buffer_offset(nvk_cmdbuf *c, uint32_t set, uint32_t index, size_t offset);

static int nvmtl_bind_slot(nvk_cmdbuf *c, uint32_t set, uint32_t binding, uint8_t kind, void *h, size_t off)
{
    if (set > 1 || binding >= NVMTL_NBIND) return -1;
    if (!c->table) {
        c->table = calloc(1, sizeof(nvmtl_bindtable));
        if (!c->table) { nvlog("vk: out of memory for the binding table"); return -1; }
    }
    nvmtl_bindtable *t = (nvmtl_bindtable *)c->table;
    nvmtl_slot *sl = &t->s[set][binding];
    if (sl->kind != kind || sl->h != h || sl->off != off) {
        t->dirty[set] = 1;
        t->chg[set][binding >> 6] |= 1ull << (binding & 63);
        if (kind != NVMTL_SLOT_BUF || sl->kind != NVMTL_SLOT_BUF) t->texgen[set]++;
    }
    sl->kind = kind; sl->h = h; sl->off = off;
    return 0;
}

int nvmtl_vk_emb_same(nvk_cmdbuf *c, uint32_t set, const void *fn, const uint64_t *key, uint32_t n)
{
    nvmtl_bindtable *t = c ? (nvmtl_bindtable *)c->table : NULL;
    if (!t || set > 1 || !fn || n > NVMTL_EMB_KEYMAX || !nvmtl_dirty_only_on()) return 0;
    return t->emb_fn[set] == fn && t->emb_gen[set] == t->texgen[set] && t->emb_n[set] == n && !memcmp(t->emb_key[set], key, (size_t)n * 8);
}
void nvmtl_vk_emb_note(nvk_cmdbuf *c, uint32_t set, const void *fn, const uint64_t *key, uint32_t n)
{
    nvmtl_bindtable *t = c ? (nvmtl_bindtable *)c->table : NULL;
    if (!t || set > 1) return;
    if (!fn || n > NVMTL_EMB_KEYMAX) { t->emb_fn[set] = NULL; return; }
    t->emb_fn[set] = fn; t->emb_gen[set] = t->texgen[set]; t->emb_n[set] = n; memcpy(t->emb_key[set], key, (size_t)n * 8);
}
int nvmtl_vk_bind_buffer_offset(nvk_cmdbuf *c, uint32_t set, uint32_t index, size_t offset)
{
    if (set > 1 || index >= NVMTL_NBUF || !c->table) return -1;
    nvmtl_bindtable *t = (nvmtl_bindtable *)c->table;
    if (t->s[set][index].kind != NVMTL_SLOT_BUF) return -1;
    if (offset >= t->s[set][index].size) { nvlog("buffer offset %zu exceeds buffer length %zu", offset, t->s[set][index].size); return -1; }
    if (t->s[set][index].off != offset) t->dirty[set] = 1;
    t->s[set][index].off = offset;
    return 0;
}

#define NVMTL_TABLES_PER_PAGE 256
#define NVMTL_TABLE_BYTES ((2 * NVMTL_NBUF + NVMTL_NTEX) * sizeof(uint64_t))
typedef struct nvmtl_address_page { nvk_buffer buffer; struct nvmtl_address_page *next; } nvmtl_address_page;
#define NVMTL_KEEP 256
static pthread_mutex_t g_keep_lock = PTHREAD_MUTEX_INITIALIZER;
static VkCommandPool g_keep_pool[NVMTL_KEEP]; static unsigned g_keep_npool;
static nvmtl_address_page *g_keep_page[NVMTL_KEEP]; static unsigned g_keep_npage;
static int nvmtl_keep_on(void)
{
    static int on = -1;
    if (on < 0) {
        const char *e = getenv("NVMTL_POOL_REUSE");
        on = (!e || e[0] == '1') && getenv("NVMTL_NO_POOL_REUSE") == NULL;
        if (on) nvlog("vk: pool reuse ON (witness-guarded default) - empty command pools and address pages kept (<= %d), reused only on THE WITNESS", NVMTL_KEEP);
        else nvlog("vk: pool reuse OFF by environment - command pools and address pages are created and destroyed per commit");
    }
    return on;
}
static VkCommandPool nvmtl_keep_pool_pop(void)
{
    VkCommandPool p = VK_NULL_HANDLE;
    if (!nvmtl_keep_on()) return p;
    pthread_mutex_lock(&g_keep_lock); if (g_keep_npool) p = g_keep_pool[--g_keep_npool]; pthread_mutex_unlock(&g_keep_lock);
    return p;
}
static int nvmtl_keep_pool_push(VkCommandPool p)
{
    int kept = 0;
    if (!nvmtl_keep_on()) return 0;
    pthread_mutex_lock(&g_keep_lock); if (g_keep_npool < NVMTL_KEEP) { g_keep_pool[g_keep_npool++] = p; kept = 1; } pthread_mutex_unlock(&g_keep_lock);
    return kept;
}
static nvmtl_address_page *nvmtl_keep_page_pop(void)
{
    nvmtl_address_page *p = NULL;
    if (!nvmtl_keep_on()) return p;
    pthread_mutex_lock(&g_keep_lock); if (g_keep_npage) p = g_keep_page[--g_keep_npage]; pthread_mutex_unlock(&g_keep_lock);
    return p;
}
static int nvmtl_keep_page_push(nvmtl_address_page *p)
{
    int kept = 0;
    if (!nvmtl_keep_on()) return 0;
    pthread_mutex_lock(&g_keep_lock); if (g_keep_npage < NVMTL_KEEP) { p->next = NULL; g_keep_page[g_keep_npage++] = p; kept = 1; } pthread_mutex_unlock(&g_keep_lock);
    return kept;
}
#define NVMTL_WIT_SLOTS 4096u
#define NVMTL_WSEQ_BLIND 0xFFFFFFFFu
static nvk_buffer g_wit; static int g_wit_state;
static uint32_t g_wit_seq;
static uint64_t g_wit_ok, g_wit_waited, g_wit_leak, g_wit_blind, g_wit_ctl_run, g_wit_ctl_fired;
static int nvmtl_wit_ready(void)
{
    int st = __atomic_load_n(&g_wit_state, __ATOMIC_ACQUIRE);
    if (st) return st > 0;
    pthread_mutex_lock(&g_keep_lock);
    st = g_wit_state;
    if (!st) {
        if (nvmtl_vk_buffer_create(NVMTL_WIT_SLOTS * 4, 1, &g_wit) == 0 && g_wit.map) { memset(g_wit.map, 0, NVMTL_WIT_SLOTS * 4); st = 1; }
        else { nvmtl_vk_buffer_destroy(&g_wit); st = -1;
               nvlog("vk: keep witness buffer REFUSED - command pools and address pages go back to create/destroy per commit"); }
        __atomic_store_n(&g_wit_state, st, __ATOMIC_RELEASE);
    }
    pthread_mutex_unlock(&g_keep_lock);
    return st > 0;
}
static uint32_t nvmtl_wit_read(uint32_t s) { return ((volatile uint32_t *)g_wit.map)[s % NVMTL_WIT_SLOTS]; }
static uint32_t nvmtl_wit_record(nvk_cmdbuf *c)
{
    if (c->in_rp || !nvmtl_wit_ready()) { __atomic_add_fetch(&g_wit_blind, 1, __ATOMIC_RELAXED); return NVMTL_WSEQ_BLIND; }
    uint32_t s;
    do s = __atomic_add_fetch(&g_wit_seq, 1, __ATOMIC_RELAXED); while (s == 0 || s == NVMTL_WSEQ_BLIND);
    VkMemoryBarrier toT = { VK_STRUCTURE_TYPE_MEMORY_BARRIER, NULL, VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_TRANSFER_WRITE_BIT };
    pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 1, &toT, 0, NULL, 0, NULL);
    pvkCmdFillBuffer(c->cb, (VkBuffer)g_wit.buf, (VkDeviceSize)(s % NVMTL_WIT_SLOTS) * 4, 4, s);
    VkMemoryBarrier toH = { VK_STRUCTURE_TYPE_MEMORY_BARRIER, NULL, VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_HOST_READ_BIT };
    pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_HOST_BIT, 0, 1, &toH, 0, NULL, 0, NULL);
    return s;
}
static void nvmtl_wit_control(uint32_t s)
{
    static int on = -1; if (on < 0) { const char *e = getenv("NVMTL_KEEP_WITNESS_CONTROL"); on = e && e[0] == '1'; }
    if (!on || s == 0 || s == NVMTL_WSEQ_BLIND) return;
    __atomic_add_fetch(&g_wit_ctl_run, 1, __ATOMIC_RELAXED);
    if (nvmtl_wit_read(s) != s) __atomic_add_fetch(&g_wit_ctl_fired, 1, __ATOMIC_RELAXED);
    else nvlog("vk: keep witness CONTROL DID NOT FIRE - slot already held %u before its submit; the witness proves nothing", s);
}
static int nvmtl_keep_verdict(uint32_t s)
{
    if (!nvmtl_keep_on() || s == NVMTL_WSEQ_BLIND) return 0;
    if (s == 0) return 1;
    if (g_state != 1 || g_wit_state <= 0) return -1;
    if (nvmtl_wit_read(s) == s) { __atomic_add_fetch(&g_wit_ok, 1, __ATOMIC_RELAXED); return 1; }
    static long wait_ms = -1; if (wait_ms < 0) { const char *e = getenv("NVMTL_WIT_WAIT_MS"); wait_ms = e ? atol(e) : 1000; }
    const uint64_t w0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    for (;;) {
        const uint32_t v = nvmtl_wit_read(s);
        const uint64_t us = (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - w0) / 1000;
        if (v == s) {
            if (!__atomic_fetch_add(&g_wit_waited, 1, __ATOMIC_RELAXED))
                nvlog("vk: keep witness %u landed only %llu us AFTER the flush returned - the flush is not synchronous any more", s, (unsigned long long)us);
            return 1;
        }
        if (us >= (uint64_t)wait_ms * 1000) {
            if (!__atomic_fetch_add(&g_wit_leak, 1, __ATOMIC_RELAXED))
                nvlog("vk: keep witness %u NOT seen after %ld ms (slot holds %u) - pool and pages LEAKED, not reused", s, wait_ms, v);
            return -1;
        }
        if (us < 1000) sched_yield(); else usleep(50);
    }
}
static int g_perf_on = -1;
static pthread_mutex_t g_perf_lock = PTHREAD_MUTEX_INITIALIZER;
typedef struct { uint64_t n, ns, bytes; } nvmtl_perf_acc;
static nvmtl_perf_acc g_perf[NVP_N]; static uint64_t g_perf_last;
static __thread int g_perf_kind;
uint64_t nvmtl_perf_now(void)
{
    if (g_perf_on < 0) { const char *e = getenv("NVMTL_PERF_LOG"); g_perf_on = e && e[0] && e[0] != '0'; }
    return g_perf_on ? clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) : 0;
}
void nvmtl_perf_note(int what, uint64_t t0, uint64_t bytes)
{
    if (!t0 || what < 0 || what >= NVP_N) return;
    const uint64_t dt = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0;
    pthread_mutex_lock(&g_perf_lock); g_perf[what].n++; g_perf[what].ns += dt; g_perf[what].bytes += bytes; pthread_mutex_unlock(&g_perf_lock);
}
static void nvmtl_perf_submit(unsigned sets, const uint64_t *pt)
{
    if (!pt[0]) return;
    static const char *const nm[NVP_N] = { "begin", "bufnew", "bufpool", "bufdel", "heap", "syncin", "syncout", "copy" };
    char s[640]; int k = 0; s[0] = 0;
    pthread_mutex_lock(&g_perf_lock);
    const double gap = !g_perf_kind && g_perf_last ? (double)(pt[0] - g_perf_last) / 1e3 : 0;
    for (int i = 0; i < NVP_N; i++) if (g_perf[i].n && k < (int)sizeof s - 64)
        k += snprintf(s + k, sizeof s - (size_t)k, " %s %llu:%.1f:%lluK", nm[i], (unsigned long long)g_perf[i].n,
                      (double)g_perf[i].ns / 1e3, (unsigned long long)(g_perf[i].bytes >> 10));
    memset(g_perf, 0, sizeof g_perf);
    if (!g_perf_kind) g_perf_last = pt[7];
    pthread_mutex_unlock(&g_perf_lock);
    fprintf(stderr, "PERFSUB k%d sets %u gap %.1f hook %.1f end %.1f fence %.1f submit %.1f wait %.1f retire %.1f wit %llu:%llu:%llu:%llu ctl %llu/%llu |%s\n",
            g_perf_kind, sets, gap, (double)(pt[1] - pt[0]) / 1e3, (double)(pt[2] - pt[1]) / 1e3, (double)(pt[3] - pt[2]) / 1e3,
            (double)(pt[4] - pt[3]) / 1e3, (double)(pt[5] - pt[4]) / 1e3, (double)(pt[7] - pt[6]) / 1e3,
            (unsigned long long)g_wit_ok, (unsigned long long)g_wit_waited, (unsigned long long)g_wit_leak,
            (unsigned long long)g_wit_blind, (unsigned long long)g_wit_ctl_fired, (unsigned long long)g_wit_ctl_run, s);
}
static int nvmtl_address_snapshot(nvk_cmdbuf *c, uint32_t which, VkDescriptorBufferInfo *out) {
    unsigned slot = c->address_count % NVMTL_TABLES_PER_PAGE;
    nvmtl_address_page *page = c->address_pages;
    if (slot == 0) {
        page = nvmtl_keep_page_pop();
        if (!page) {
            page = calloc(1, sizeof(*page));
            if (!page) return -1;
            if (nvmtl_vk_buffer_create(NVMTL_TABLE_BYTES * NVMTL_TABLES_PER_PAGE, 1, &page->buffer)) { nvmtl_vk_buffer_destroy(&page->buffer); free(page); return -1; }
        }
        page->next = c->address_pages; c->address_pages = page;
    }
    uint64_t *addresses = (uint64_t *)((uint8_t *)page->buffer.map + slot * NVMTL_TABLE_BYTES);
    nvmtl_bindtable *t = c->table;
    for (unsigned i = 0; i < NVMTL_NBUF; ++i) {
        nvmtl_slot *s = t ? &t->s[which][i] : NULL;
        addresses[i] = s && s->kind == NVMTL_SLOT_BUF ? s->address + s->off : nvmtl_vk_buffer_address(&g_dummyBuf);
    }
    uint64_t self = nvmtl_vk_buffer_address(&page->buffer);
    if (!self) nvlog("address table: page has no device address - array_ref<void> arguments will read 0");
    else self += slot * NVMTL_TABLE_BYTES;
    for (unsigned i = 0; i < NVMTL_NBUF; ++i) addresses[NVMTL_NBUF + i] = self ? self + i * sizeof(uint64_t) : 0;
    for (unsigned i = 0; i < NVMTL_NTEX; ++i) {
        const nvmtl_slot *s = t ? &t->s[which][NVMTL_NBUF + i] : NULL;
        addresses[2 * NVMTL_NBUF + i] = (s && s->kind == NVMTL_SLOT_TEX && s->h) ? (uint64_t)s->size : 0;
    }
    *out = (VkDescriptorBufferInfo){ page->buffer.buf, slot * NVMTL_TABLE_BYTES, NVMTL_TABLE_BYTES };
    c->address_count++;
    return 0;
}

static void nvmtl_spirv_retarget_set(uint32_t *w, size_t words, uint32_t newSet)
{
    if (words < 5 || w[0] != 0x07230203u) return;
    size_t i = 5;
    while (i < words) {
        uint32_t count = w[i] >> 16, op = w[i] & 0xffff;
        if (count == 0 || i + count > words) return;
        if (op == 71  && count >= 4 && w[i + 2] == 34  && w[i + 3] == 0) w[i + 3] = newSet;
        i += count;
    }
}

static uint32_t nvmtl_spirv_has_specid(const void *code, size_t words, uint32_t id)
{
    const uint32_t *w = code;
    for (size_t i = 5; i < words; ) {
        uint32_t wc = w[i] >> 16, op = w[i] & 0xffff;
        if (!wc || i + wc > words) return 0;
        if (op == 71 && wc >= 4 && w[i + 2] == 1 && w[i + 3] == id) return 1;
        if (op == 54) return 0;
        i += wc;
    }
    return 0;
}
static int nvmtl_spirv_uses_set(const void *code, size_t words, uint32_t set)
{
    const uint32_t *w = (const uint32_t *)code;
    if (!w || words < 5 || w[0] != 0x07230203u) return 0;
    for (size_t i = 5; i < words; ) {
        uint32_t count = w[i] >> 16, op = w[i] & 0xffff;
        if (count == 0 || i + count > words) return 0;
        if (op == 71 && count >= 4 && w[i + 2] == 34 && w[i + 3] == set) return 1;
        i += count;
    }
    return 0;
}

#define NVMTL_DESC_BATCH_MAX 32
static unsigned nvmtl_desc_batch(void)
{
    static int n = -1;
    if (n < 0) {
        const char *e = getenv("NVMTL_DESC_BATCH");
        n = e && *e ? atoi(e) : NVMTL_DESC_BATCH_MAX;
        if (n < 1) n = 1;
        if (n > NVMTL_DESC_BATCH_MAX) n = NVMTL_DESC_BATCH_MAX;
    }
    return (unsigned)n;
}
static VkDescriptorSet nvmtl_snapshot_set_m(nvk_cmdbuf *c, uint32_t which, uint32_t stex_hi, const uint64_t *mask, const uint64_t *tb);
static VkDescriptorSet nvmtl_snapshot_set(nvk_cmdbuf *c, uint32_t which, uint32_t stex_hi)
{
    return nvmtl_snapshot_set_m(c, which, stex_hi, NULL, NULL);
}
static VkDescriptorSet nvmtl_snapshot_set_t(nvk_cmdbuf *c, uint32_t which, uint32_t stex_hi, const uint64_t *tb)
{
    return nvmtl_snapshot_set_m(c, which, stex_hi, NULL, tb);
}
#define NVMTL_E78_HAS(m, i) (!(m) || (((m)[(i) >> 6] >> ((i) & 63)) & 1))
static VkDescriptorSet nvmtl_snapshot_set_md(nvk_cmdbuf *c, uint32_t which, uint32_t stex_hi, const uint64_t *mask, const uint64_t *tb,
                                             VkDescriptorSet from, const uint64_t *copym);
static VkDescriptorSet nvmtl_snapshot_set_m(nvk_cmdbuf *c, uint32_t which, uint32_t stex_hi, const uint64_t *mask, const uint64_t *tb)
{ return nvmtl_snapshot_set_md(c, which, stex_hi, mask, tb, VK_NULL_HANDLE, NULL); }
#define NVMTL_COPY_HAS(m, i) ((m) && (((m)[(i) >> 6] >> ((i) & 63)) & 1))
static VkDescriptorSet nvmtl_snapshot_set_md(nvk_cmdbuf *c, uint32_t which, uint32_t stex_hi, const uint64_t *mask, const uint64_t *tb,
                                             VkDescriptorSet from, const uint64_t *copym)
{
    VkDescriptorSet set = VK_NULL_HANDLE; VkDescriptorPool pool = VK_NULL_HANDLE;
    VkResult r = VK_ERROR_OUT_OF_POOL_MEMORY;
    if (c->ndcache) {
        set = (VkDescriptorSet)c->dcache[--c->ndcache];
        pool = (VkDescriptorPool)c->dcachepool;
        r = VK_SUCCESS;
        if (c->nsets == c->capsets) {
            unsigned cap = c->capsets ? c->capsets * 2 : 64;
            nvk_setref *grown = (nvk_setref *)realloc(c->sets, cap * sizeof *grown);
            if (grown) { c->sets = grown; c->capsets = cap; }
            else { c->dcache[c->ndcache++] = (void *)set; c->skipped++; return VK_NULL_HANDLE; }
        }
    } else {
        pthread_mutex_lock(&g_desc_lock);
        if (ensure_descriptors_locked()) { pthread_mutex_unlock(&g_desc_lock); c->skipped++; return VK_NULL_HANDLE; }
        unsigned n = nvmtl_desc_batch();
        VkDescriptorSetLayout ls[NVMTL_DESC_BATCH_MAX];
        VkDescriptorSet got[NVMTL_DESC_BATCH_MAX];
        for (unsigned k = 0; k < NVMTL_DESC_BATCH_MAX; k++) ls[k] = g_setLayout;
        for (;;) {
            r = VK_ERROR_OUT_OF_POOL_MEMORY;
            for (unsigned k = g_ndescPools; k-- > 0 && r != VK_SUCCESS;) {
                VkDescriptorSetAllocateInfo ai = { VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, NULL, g_descPools[k], n, ls };
                pool = g_descPools[k]; r = pvkAllocateDescriptorSets(g_dev, &ai, got);
            }
            if (r != VK_SUCCESS && nvmtl_desc_pool_open() == 0) {
                VkDescriptorSetAllocateInfo ai = { VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, NULL, g_descPools[g_ndescPools - 1], n, ls };
                pool = g_descPools[g_ndescPools - 1]; r = pvkAllocateDescriptorSets(g_dev, &ai, got);
            }
            if (r == VK_SUCCESS || n == 1) break;
            n = 1;
        }
        if (r == VK_SUCCESS) {
            set = got[0];
            c->dcachepool = (void *)pool;
            for (unsigned k = 1; k < n; k++) c->dcache[c->ndcache++] = (void *)got[k];
        }
        if (r == VK_SUCCESS && c->nsets == c->capsets) {
            unsigned cap = c->capsets ? c->capsets * 2 : 64;
            nvk_setref *grown = (nvk_setref *)realloc(c->sets, cap * sizeof *grown);
            if (grown) { c->sets = grown; c->capsets = cap; }
            else {
                pvkFreeDescriptorSets(g_dev, pool, 1, &set);
                if (c->ndcache) {
                    VkDescriptorSet back[NVMTL_DESC_BATCH_MAX];
                    for (unsigned k = 0; k < c->ndcache; k++) back[k] = (VkDescriptorSet)c->dcache[k];
                    pvkFreeDescriptorSets(g_dev, pool, c->ndcache, back);
                    c->ndcache = 0; c->dcachepool = NULL;
                }
                r = VK_ERROR_OUT_OF_HOST_MEMORY;
            }
        }
        pthread_mutex_unlock(&g_desc_lock);
    }
    if (r != VK_SUCCESS) {
        nvlog("vk: descriptor set allocation FAILED (%d) with %u pools — this draw is SKIPPED, not recorded blind", r, g_ndescPools);
        c->skipped++; return VK_NULL_HANDLE;
    }
    c->sets[c->nsets++] = (nvk_setref){ pool, set };
    VkWriteDescriptorSet w[NVMTL_NBIND];
    VkCopyDescriptorSet cp[NVMTL_NBIND]; uint32_t ncp = 0;
    if (!from) copym = NULL;
    VkDescriptorBufferInfo bi[NVMTL_NBIND];
    VkDescriptorImageInfo  ii[NVMTL_NBIND];
    VkBufferView tv[NVMTL_NBIND];
    VkDescriptorBufferInfo addressInfo;
    if (!g_mutable) tb = NULL;
    if (NVMTL_E78_HAS(mask, NVMTL_ADDRESS_BINDING) && nvmtl_address_snapshot(c, which, &addressInfo)) { c->skipped++; return VK_NULL_HANDLE; }
    VkDescriptorBufferInfo dummyB = { (VkBuffer)g_dummyBuf.buf, 0, VK_WHOLE_SIZE };
    VkDescriptorImageInfo  dummyI = { VK_NULL_HANDLE, (VkImageView)g_dummyImg.view, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL };
    VkDescriptorImageInfo  dummyS = { g_dummySampler, VK_NULL_HANDLE, VK_IMAGE_LAYOUT_UNDEFINED };
    nvmtl_bindtable *t = (nvmtl_bindtable *)c->table;
    uint32_t nw = 0;
    for (uint32_t i = 0; i < NVMTL_STEX_BASE; i++) {
        if (!NVMTL_E78_HAS(mask, i)) continue;
        VkDescriptorType ty = binding_type(i);
        const nvmtl_slot *sl = t ? &t->s[which][i] : NULL;
        const VkDescriptorBufferInfo *pb = NULL; const VkDescriptorImageInfo *pi = NULL;
        if (ty == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER || ty == VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC) {
            if (i == NVMTL_ADDRESS_BINDING) pb = &addressInfo;
            else if (sl && sl->kind == NVMTL_SLOT_BUF && (sl->off % NVMTL_SSBO_ALIGN) == 0) {
                bi[i] = (VkDescriptorBufferInfo){ (VkBuffer)sl->h, sl->off, VK_WHOLE_SIZE }; pb = &bi[i];
            } else {
                if (sl && sl->kind == NVMTL_SLOT_BUF) { static int said; if (!said++) nvlog("vk: buffer slot %u is bound at offset %zu, not a multiple of %d — reachable only through the address table", i, sl->off, NVMTL_SSBO_ALIGN); }
                pb = &dummyB;
            }
        } else if (ty == VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE) {
            const uint32_t k = i - NVMTL_NBUF; const int wantT = tb && ((tb[k >> 6] >> (k & 63)) & 1);
            if (wantT) {
                tv[i] = (VkBufferView)(sl && sl->kind == NVMTL_SLOT_TEXBUF && sl->h ? sl->h : g_dummyTexView);
                w[nw++] = (VkWriteDescriptorSet){ VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, NULL, set, NVMTL_BINDING_NUMBER(i), 0, 1, VK_DESCRIPTOR_TYPE_UNIFORM_TEXEL_BUFFER, NULL, NULL, &tv[i] };
                continue;
            }
            if (NVMTL_COPY_HAS(copym, i)) { cp[ncp++] = (VkCopyDescriptorSet){ VK_STRUCTURE_TYPE_COPY_DESCRIPTOR_SET, NULL, from, NVMTL_BINDING_NUMBER(i), 0, set, NVMTL_BINDING_NUMBER(i), 0, 1 }; continue; }
            if (sl && sl->kind == NVMTL_SLOT_TEX) { ii[i] = (VkDescriptorImageInfo){ VK_NULL_HANDLE, (VkImageView)sl->h, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL }; pi = &ii[i]; }
            else pi = &dummyI;
        } else {
            if (NVMTL_COPY_HAS(copym, i)) { cp[ncp++] = (VkCopyDescriptorSet){ VK_STRUCTURE_TYPE_COPY_DESCRIPTOR_SET, NULL, from, NVMTL_BINDING_NUMBER(i), 0, set, NVMTL_BINDING_NUMBER(i), 0, 1 }; continue; }
            if (sl && sl->kind == NVMTL_SLOT_SAMP) { ii[i] = (VkDescriptorImageInfo){ (VkSampler)sl->h, VK_NULL_HANDLE, VK_IMAGE_LAYOUT_UNDEFINED }; pi = &ii[i]; }
            else pi = &dummyS;
        }
        w[nw++] = (VkWriteDescriptorSet){ VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, NULL, set, NVMTL_BINDING_NUMBER(i), 0, 1, ty, pi, pb, NULL };
    }
    if (stex_hi > NVMTL_NSTEX) stex_hi = NVMTL_NSTEX;
    for (uint32_t j = 0; j < stex_hi; j++) {
        uint32_t i = NVMTL_STEX_BASE + j;
        const nvmtl_slot *sl = t ? &t->s[which][i] : NULL;
        if (tb && ((tb[2 + (j >> 6)] >> (j & 63)) & 1)) {
            tv[i] = (VkBufferView)(sl && sl->kind == NVMTL_SLOT_TEXBUF && sl->h ? sl->h : g_dummyTexView);
            w[nw++] = (VkWriteDescriptorSet){ VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, NULL, set, NVMTL_BINDING_NUMBER(i), 0, 1, VK_DESCRIPTOR_TYPE_STORAGE_TEXEL_BUFFER, NULL, NULL, &tv[i] };
            continue;
        }
        if (NVMTL_COPY_HAS(copym, i)) { cp[ncp++] = (VkCopyDescriptorSet){ VK_STRUCTURE_TYPE_COPY_DESCRIPTOR_SET, NULL, from, NVMTL_BINDING_NUMBER(i), 0, set, NVMTL_BINDING_NUMBER(i), 0, 1 }; continue; }
        int bound = sl && sl->kind == NVMTL_SLOT_TEX && sl->h;
        void *view = bound ? sl->h : g_dummyStex.view;
        if (!bound) { static int said; if (!said++) nvlog("vk: storage texture %u is declared by the shader (through %u) but nothing writable is bound there — %s", j, stex_hi - 1, view ? "dummy" : "left UNWRITTEN"); }
        if (!view) continue;
        ii[i] = (VkDescriptorImageInfo){ VK_NULL_HANDLE, (VkImageView)view, VK_IMAGE_LAYOUT_GENERAL };
        w[nw++] = (VkWriteDescriptorSet){ VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, NULL, set, NVMTL_BINDING_NUMBER(i), 0, 1, VK_DESCRIPTOR_TYPE_STORAGE_IMAGE, &ii[i], NULL, NULL };
    }
    nvmtl_enc78_count(E78C_WRITES, nw);
    pvkUpdateDescriptorSets(g_dev, nw, w, ncp, cp);
    return set;
}
static VkDescriptorSet nvmtl_snapshot_set_c(nvk_cmdbuf *c, uint32_t which, const nvk_pipeline *p)
{
    nvmtl_bindtable *t = (nvmtl_bindtable *)c->table;
    if (!t || which > 1 || !nvmtl_enc78_on(NVMTL_E78_H3) || p->tbany) return nvmtl_snapshot_set_t(c, which, p->stex_hi[which], p->tbany ? p->tbm[which] : NULL);
    const uint64_t all[4] = { ~0ull, ~0ull, ~0ull, ~0ull };
    const uint64_t *pm = p->bmask_ok ? p->bmask : (p->gmask_ok && which < 2) ? p->gmask[which] : all;
    uint32_t hi = p->stex_hi[which];
    int live = t->last[which] && !t->dirty[which] && t->last_idx[which] < c->nsets
               && (VkDescriptorSet)c->sets[t->last_idx[which]].set == t->last[which];
    if (live && hi <= t->last_hi[which]) {
        int covered = 1;
        for (int k = 0; k < 4; k++) if (pm[k] & ~t->lastw[which][k]) covered = 0;
        if (covered) { nvmtl_enc78_count(E78C_SETREUSE, 1); return t->last[which]; }
    }
    uint64_t m[4];
    for (int k = 0; k < 4; k++) m[k] = pm[k] | (live ? t->lastw[which][k] : 0);
    if (live && t->last_hi[which] > hi) hi = t->last_hi[which];
    VkDescriptorSet from = VK_NULL_HANDLE; uint64_t cm[4] = { 0, 0, 0, 0 };
    if (live && nvmtl_dirty_only_on()) {
        from = t->last[which];
        for (int k = 0; k < 4; k++) cm[k] = t->lastw[which][k];
        for (uint32_t j = 0; j < NVMTL_NSTEX && NVMTL_STEX_BASE + j < 256; j++) {
            const uint32_t i = NVMTL_STEX_BASE + j;
            if (j < t->last_hi[which]) cm[i >> 6] |= 1ull << (i & 63); else cm[i >> 6] &= ~(1ull << (i & 63));
        }
        for (int k = 0; k < 4; k++) cm[k] &= ~t->chg[which][k];
    }
    VkDescriptorSet s = nvmtl_snapshot_set_md(c, which, hi, m, NULL, from, from ? cm : NULL);
    if (!s) return s;
    t->last[which] = s; t->last_idx[which] = c->nsets - 1; t->last_hi[which] = hi; t->dirty[which] = 0;
    memset(t->chg[which], 0, sizeof t->chg[which]);
    memcpy(t->lastw[which], m, sizeof m);
    nvmtl_enc78_count(E78C_SETNEW, 1);
    return s;
}

int nvmtl_vk_bind_buffer(nvk_cmdbuf *c, uint32_t set, uint32_t index, nvk_buffer *b, size_t offset)
{
    if (index >= NVMTL_NBUF) { nvlog("bind buffer: index %u is outside [0,%d)", index, NVMTL_NBUF); return -1; }
    if (!b || !b->buf || offset >= b->size) { nvlog("bind buffer: offset %zu outside length %zu", offset, b ? b->size : 0); return -1; }
    int r = nvmtl_bind_slot(c, set, index, NVMTL_SLOT_BUF, b->buf, offset);
    if (!r) { nvmtl_bindtable *t = (nvmtl_bindtable *)c->table; nvmtl_slot *sl = &t->s[set][index]; uint64_t a = nvmtl_vk_buffer_address(b);
              if (sl->size != b->size || sl->address != a) t->dirty[set] = 1;
              sl->size = b->size; sl->address = a; }
    return r;
}
int nvmtl_vk_trace_buffers(nvk_cmdbuf *c, char *out, size_t cap)
{
    if (!out || cap < 16) return 0;
    out[0] = 0;
    if (!c || !c->table) { snprintf(out, cap, " (no bind table)"); return 0; }
    nvmtl_bindtable *t = (nvmtl_bindtable *)c->table;
    size_t n = 0, lim = cap - 8;
    int k = 0;
    for (int set = 0; set < 2; set++)
        for (int i = 0; i < NVMTL_NBUF; i++) {
            nvmtl_slot *s = &t->s[set][i];
            if (s->kind != NVMTL_SLOT_BUF) continue;
            int w = snprintf(out + n, lim - n, " %c%d 0x%llx+%zu/%zu", set ? 'f' : 'v', i,
                             (unsigned long long)s->address, s->off, s->size);
            if (w < 0 || (size_t)w >= lim - n) { out[n] = 0; snprintf(out + n, cap - n, " +more"); return k; }
            n += (size_t)w; k++;
        }
    if (!k) snprintf(out, cap, " (none)");
    return k;
}
int nvmtl_vk_bind_texture(nvk_cmdbuf *c, uint32_t set, uint32_t index, nvk_image *img)
{
    if (index >= NVMTL_NTEX) { nvlog("bind texture: index %u is outside [0,%d)", index, NVMTL_NTEX); return -1; }
    return nvmtl_bind_slot(c, set, NVMTL_NBUF + index, NVMTL_SLOT_TEX, img->view, 0);
}
int nvmtl_vk_bind_sampler(nvk_cmdbuf *c, uint32_t set, uint32_t index, nvk_sampler *s)
{
    if (index >= NVMTL_NSAMP) { nvlog("bind sampler: index %u is outside [0,%d)", index, NVMTL_NSAMP); return -1; }
    return nvmtl_bind_slot(c, set, NVMTL_NBUF + NVMTL_NTEX + index, s ? NVMTL_SLOT_SAMP : NVMTL_SLOT_NONE, s ? s->s : NULL, 0);
}

int nvmtl_vk_sample_positions_backend(void) { return nvmtl_vk_init() == 0 && g_sample_locations; }
int nvmtl_vk_sample_positions_default(uint32_t count, nvmtl_sample_pattern *out)
{
    if (!nvmtl_vk_sample_positions_backend() || !(g_sample_props.sampleLocationSampleCounts & count)) return -1;
    return nvmtl_sample_default(count, out);
}
static VkSampleLocationsInfoEXT nvmtl_sample_info(const nvmtl_sample_pattern *p, VkSampleLocationEXT vk[8])
{
    for (uint32_t i = 0; i < p->count; ++i) vk[i] = (VkSampleLocationEXT){ nvmtl_sample_raster_coordinate(p->position[i].x), nvmtl_sample_raster_coordinate(p->position[i].y) };
    return (VkSampleLocationsInfoEXT){ VK_STRUCTURE_TYPE_SAMPLE_LOCATIONS_INFO_EXT, NULL,
        (VkSampleCountFlagBits)p->count, {1,1}, p->count, vk };
}

void nvmtl_vk_cmd_bind_set(nvk_cmdbuf *c, nvk_pipeline *p)
{
    VkDescriptorSet s[3] = { nvmtl_snapshot_set_c(c, 0, p), nvmtl_snapshot_set_c(c, 1, p), g_heapSet };
    if (!s[0] || !s[1]) { c->nobind = 1; return; }
    c->nobind = 0;
    if (g_sample_locations && c->sample_pattern.count)
        pvkCmdPushConstants(c->cb, (VkPipelineLayout)p->layout, VK_SHADER_STAGE_FRAGMENT_BIT, NVMTL_SAMPLE_POSITION_OFFSET,
                           NVMTL_SAMPLE_POSITION_BYTES, c->sample_pattern.position);
    pvkCmdBindDescriptorSets(c->cb, VK_PIPELINE_BIND_POINT_GRAPHICS, (VkPipelineLayout)p->layout, 0, g_heapSet ? 3 : 2, s, nvmtl_table_ubo() ? 2 : 0, g_dyn_zero);
}

#define NVMTL_DEPTH_USAGE (VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT | VK_IMAGE_USAGE_SAMPLED_BIT | \
                           VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT)
int nvmtl_vk_depth_create(uint32_t w, uint32_t h, nvk_image *out)
{
    return nvmtl_vk_depth_create_ex(w, h, VK_FORMAT_D32_SFLOAT, VK_IMAGE_ASPECT_DEPTH_BIT, out);
}
int nvmtl_vk_depth_create_ex(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t aspect, nvk_image *out)
{ return nvmtl_vk_depth_create_ms(w, h, vkfmt, aspect, 1, out); }
int nvmtl_vk_depth_create_ms(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t aspect, uint32_t samples, nvk_image *out)
{ return nvmtl_vk_depth_create_full(w, h, vkfmt, aspect, samples, samples > 1 ? 4u : 2u, 1u, 1u, out); }
static int nvmtl_vk_depth_create_full_lifetime_body(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t aspect, uint32_t samples, uint32_t mtl_type,
                               uint32_t layers, uint32_t mips, nvk_image *out);
int nvmtl_vk_depth_create_full(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t aspect, uint32_t samples, uint32_t mtl_type,
                               uint32_t layers, uint32_t mips, nvk_image *out)
{
    nvk_image pending = {0};
    int result = nvmtl_vk_depth_create_full_lifetime_body(w, h, vkfmt, aspect, samples, mtl_type, layers, mips, &pending);
    if (result) { nvmtl_failed_image(&pending); memset(out, 0, sizeof *out); }
    else { nvmtl_commit_image(&pending); *out = pending; }
    return result;
}
static int nvmtl_vk_depth_create_full_lifetime_body(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t aspect, uint32_t samples, uint32_t mtl_type,
                               uint32_t layers, uint32_t mips, nvk_image *out)
{
    if (nvmtl_vk_init()) return -1;
    if (!layers) layers = 1;
    if (!mips) mips = 1;
    if (mtl_type == 5 && layers != 6) layers = 6;
    const int cube = mtl_type == 5 || mtl_type == 6;
    memset(out, 0, sizeof *out); out->w = w; out->h = h; out->fmt = vkfmt; out->bpp = 4; out->samples = samples > 1 ? samples : 1;
    out->mtl_type = mtl_type == 3 || cube || mtl_type == 8 ? mtl_type : (samples > 1 ? 4 : 2); out->layers = layers; out->mips = mips;
    VkImageCreateInfo ici = { VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, NULL, cube ? VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT : 0, VK_IMAGE_TYPE_2D,
        (VkFormat)vkfmt, { w, h, 1 }, mips, layers, (VkSampleCountFlagBits)out->samples, VK_IMAGE_TILING_OPTIMAL,
        NVMTL_DEPTH_USAGE, VK_SHARING_MODE_EXCLUSIVE, 0, NULL, VK_IMAGE_LAYOUT_UNDEFINED };
    VKCK(nvmtl_opaque_CreateImage(g_dev, &ici, NULL, &out->img), "vkCreateImage(depth)");
    out->usage = (uint32_t)ici.usage;
    VkMemoryRequirements mr; pvkGetImageMemoryRequirements(g_dev, out->img, &mr);
    VKCK(nvmtl_alloc_opaque_image_mem(&mr, &out->mem, &out->sysmem, "depth"), "vkAllocateMemory(depth)");
    out->alloc = mr.size;
    VKCK(pvkBindImageMemory(g_dev, out->img, out->mem, 0), "vkBindImageMemory(depth)");
    VkImageViewType vt = out->mtl_type == 3 || out->mtl_type == 8 ? VK_IMAGE_VIEW_TYPE_2D_ARRAY : out->mtl_type == 5 ? VK_IMAGE_VIEW_TYPE_CUBE
                       : out->mtl_type == 6 ? VK_IMAGE_VIEW_TYPE_CUBE_ARRAY : VK_IMAGE_VIEW_TYPE_2D;
    VkImageViewCreateInfo vci = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, out->img, vt,
        (VkFormat)vkfmt, { 0 }, { (VkImageAspectFlags)aspect, 0, mips, 0, layers } };
    VKCK(nvmtl_opaque_CreateImageView(g_dev, &vci, NULL, &out->view), "vkCreateImageView(depth)");
    return 0;
}

void nvmtl_vk_cmd_set_depth(nvk_cmdbuf *c, int test, int write, int compare)
{
    pvkCmdSetDepthTestEnable(c->cb, test ? VK_TRUE : VK_FALSE);
    pvkCmdSetDepthWriteEnable(c->cb, write ? VK_TRUE : VK_FALSE);
    pvkCmdSetDepthCompareOp(c->cb, (VkCompareOp)compare);
}

int nvmtl_vk_pipeline_create_depth(const void *vs, size_t vn, const void *fs, size_t fn, int bgra, int hasDepth, nvk_pipeline *out)
{
    return nvmtl_vk_pipeline_create_ex(vs, vn, fs, fn, bgra ? VK_FORMAT_B8G8R8A8_UNORM : VK_FORMAT_R8G8B8A8_UNORM, hasDepth, out);
}
int nvmtl_vk_pipeline_create_ex(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, int hasDepth, nvk_pipeline *out)
{
    return nvmtl_vk_pipeline_create_ex2(vs, vn, fs, fn, vkfmt, hasDepth ? VK_FORMAT_D32_SFLOAT : 0, out);
}
#include "nvmtl_physical_bounds.h"
#include "nvmtl_independent_helpers.h"
static NSData *nvmtl_safe_physical_shader(const void *data, size_t len) {
    if (len % sizeof(uint32_t)) { nvlog("physical bounds: malformed shader size"); return nil; }
    NSMutableData *result = [NSMutableData dataWithBytes:data length:len];
    int n = nvmtl_relax_physical_bounds(result.mutableBytes, len / sizeof(uint32_t));
    if (n < 0) { nvlog("physical bounds: malformed shader or allocation failure"); return nil; }
    if (n) nvlog("physical bounds: relaxed %d allocation-relative access chains", n);
    return nvmtl_independent_helpers(result);
}
#include "nvmtl_readonly_metadata.h"
static int nvmtl_align_guard_on(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_NO_ALIGN_GUARD"); on = !(e && e[0] == '1');
        nvlog("align guard: %s (NVMTL_NO_ALIGN_GUARD=%s, translator %s)", on ? "ON - a misaligned binding runs the SAFE module, as Apple runs it" : "OFF",
              e ? e : "unset", xlate_lower_ex ? "reports promises" : "has NO nvmtl_lower_buffer_addresses_ex - guard inert"); }
    return on;
}
static _Atomic unsigned long long g_ag_render_safe, g_ag_untraceable, g_ag_guarded, g_ag_switched;
static int nvmtl_spirv_ssbo_table(const void *spv, size_t n)
{
    const uint32_t *w = (const uint32_t *)spv; size_t nw = n / 4;
    if (nw < 5 || w[0] != 0x07230203u) return 0;
    uint32_t tid = 0;
    for (size_t i = 5; i < nw; ) { uint32_t len = w[i] >> 16, op = w[i] & 0xffffu; if (!len || i + len > nw) return 0;
        if (op == 71 && len >= 4 && w[i + 2] == 33 && w[i + 3] == 640) tid = w[i + 1];
        i += len; }
    if (!tid) return 0;
    for (size_t i = 5; i < nw; ) { uint32_t len = w[i] >> 16, op = w[i] & 0xffffu; if (!len || i + len > nw) return 0;
        if (op == 59 && len >= 4 && w[i + 2] == tid) return w[i + 3] == 12;
        i += len; }
    return 0;
}
static _Atomic unsigned long long g_g1_refused;
static NSData *nvmtl_address_shader_g0(const void *data, size_t len, uint32_t flags, uint32_t *info);
static NSData *nvmtl_address_shader_g_uncached(const void *data, size_t len, uint32_t flags, uint32_t *info);
static NSData *nvmtl_address_shader_g(const void *data, size_t len, uint32_t flags, uint32_t *info) {
    static NSMutableDictionary *cache; static pthread_mutex_t lk = PTHREAD_MUTEX_INITIALIZER;
    NSMutableData *key = [NSMutableData dataWithBytes:&flags length:sizeof flags];
    [key appendBytes:data length:len];
    pthread_mutex_lock(&lk);
    NSArray *hit = cache[key];
    pthread_mutex_unlock(&lk);
    if (hit) {
        if (info) memcpy(info, [hit[1] bytes], 4 * sizeof *info);
        return hit[0] == (id)[NSNull null] ? nil : hit[0];
    }
    uint32_t got[4] = {0};
    NSData *d = nvmtl_address_shader_g_uncached(data, len, flags, got);
    if (info) memcpy(info, got, sizeof got);
    pthread_mutex_lock(&lk);
    if (!cache || cache.count >= 4096) cache = [NSMutableDictionary new];
    cache[key] = @[d ?: (id)[NSNull null], [NSData dataWithBytes:got length:sizeof got]];
    pthread_mutex_unlock(&lk);
    return d;
}
static NSData *nvmtl_address_shader_g_uncached(const void *data, size_t len, uint32_t flags, uint32_t *info) {
    NSData *d = nvmtl_address_shader_g0(data, len, flags, info);
    if (d && nvmtl_table_ubo() && nvmtl_spirv_ssbo_table(d.bytes, d.length)) {
        const unsigned long long k = atomic_fetch_add(&g_g1_refused, 1) + 1;
        if (k <= 16 || !(k & (k - 1))) nvlog("G1: a module still declares a STORAGE-buffer table at 640 while the table is Uniform - REFUSED (%llu so far; NVMTL_TABLE_SSBO=1 restores it)", k);
        return nil;
    }
    return d;
}
static NSData *nvmtl_address_shader_g0(const void *data, size_t len, uint32_t flags, uint32_t *info) {
    if (info) memset(info, 0, 4 * sizeof *info);
    __attribute__((objc_precise_lifetime)) NSData *prepared = nvmtl_preserve_readonly_metadata([NSData dataWithBytes:data length:len]);
    data = prepared.bytes; len = prepared.length;
    static unsigned said;
    if (!nvmtl_xlate_ready() || !xlate_lower || !xlate_free) {
        if (said++ < 40) nvlog("address lowering UNAVAILABLE (translator exports no nvmtl_lower_buffer_addresses): shader kept as-is");
        return nvmtl_safe_physical_shader(data, len);
    }
    uint8_t *out = NULL; size_t n = 0; char err[512] = {0};
    const int ubo = nvmtl_table_ubo();
    const int ex = xlate_lower_ex && (nvmtl_align_guard_on() || ubo);
    if (ex ? xlate_lower_ex(data, len, flags | (ubo ? 2u : 0u), &out, &n, info, err, sizeof err) : xlate_lower(data, len, &out, &n, err, sizeof err)) {
        if (info) memset(info, 0, 4 * sizeof *info);
        if (said++ < 40) nvlog("address lowering REFUSED, shader kept as-is: %s", err);
        return nvmtl_safe_physical_shader(data, len);
    }
    if (info && !nvmtl_align_guard_on()) memset(info, 0, 4 * sizeof *info);
    NSData *result = nvmtl_safe_physical_shader(out, n); xlate_free(out, n); return result;
}
static NSData *nvmtl_address_shader(const void *data, size_t len) {
    uint32_t info[4];
    NSData *d = nvmtl_address_shader_g(data, len, 0, info);
    if (d && (info[0] || info[2])) {
        const unsigned long long k = atomic_fetch_add(&g_ag_render_safe, 1) + 1;
        if (!(k & (k - 1))) nvlog("align guard: %llu render module(s) built SAFE - %u access(es) rested on a promised %u-byte alignment (bindings %#x%s)",
                                  k, info[3], info[1], info[0], info[2] ? ", untraceable" : "");
        d = nvmtl_address_shader_g(data, len, 1, NULL);
    }
    return d;
}
int nvmtl_vk_pipeline_create_ex2(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, nvk_pipeline *out)
{
    return nvmtl_vk_pipeline_create_blend(vs, vn, fs, fn, vkfmt, depthfmt, NULL, out);
}
static void nvmtl_spv_input_kinds(const void *spv, size_t n, uint8_t kind[32])
{
    const uint32_t *w = (const uint32_t *)spv; size_t nw = n / 4; memset(kind, 0, 32);
    if (nw < 5 || w[0] != 0x07230203u) return;
    uint32_t bound = w[3]; if (!bound || bound > (1u << 22)) return;
    uint8_t *tk = calloc(bound, 1), *loc = calloc(bound, 1);
    if (tk && loc) for (size_t i = 5; i < nw; ) {
        uint32_t len = w[i] >> 16, op = w[i] & 0xffffu; if (!len || i + len > nw) break;
        const uint32_t *a = w + i + 1;
        if (op == 71 && len >= 4 && a[0] < bound && a[1] == 30 && a[2] < 32) loc[a[0]] = (uint8_t)(a[2] + 1);
        else if (op == 22 && len >= 3 && a[0] < bound) tk[a[0]] = 1;
        else if (op == 21 && len >= 4 && a[0] < bound) tk[a[0]] = a[2] ? 2 : 3;
        else if (op == 23 && len >= 4 && a[0] < bound && a[1] < bound) tk[a[0]] = tk[a[1]];
        else if (op == 32 && len >= 4 && a[0] < bound && a[2] < bound) tk[a[0]] = tk[a[2]];
        else if (op == 59 && len >= 4 && a[0] < bound && a[1] < bound && a[2] == 1 && loc[a[1]]) kind[loc[a[1]] - 1] = tk[a[0]] ? tk[a[0]] : 1;
        i += len;
    }
    free(tk); free(loc);
}
static int nvmtl_vk_format_is_vertex(VkFormat f)
{
    static void (*fp)(VkPhysicalDevice, VkFormat, VkFormatProperties *); static int tried;
    if (!tried) { tried = 1; fp = (void (*)(VkPhysicalDevice, VkFormat, VkFormatProperties *))pvkGetInstanceProcAddr(g_inst, "vkGetPhysicalDeviceFormatProperties");
                  if (!fp) nvlog("G16 vk: vkGetPhysicalDeviceFormatProperties is missing — vertex formats are taken on trust"); }
    if (!fp) return 1;
    VkFormatProperties p; memset(&p, 0, sizeof p); fp(g_pdev, f, &p);
    return (p.bufferFeatures & VK_FORMAT_FEATURE_VERTEX_BUFFER_BIT) != 0;
}
_Static_assert(VK_FORMAT_R8G8_UINT - 2 == VK_FORMAT_R8G8_USCALED && VK_FORMAT_R8G8_SINT - 2 == VK_FORMAT_R8G8_SSCALED, "INT-2 = SCALED (8-bit)");
_Static_assert(VK_FORMAT_R8_UINT - 2 == VK_FORMAT_R8_USCALED && VK_FORMAT_R8G8B8A8_SINT - 2 == VK_FORMAT_R8G8B8A8_SSCALED, "INT-2 = SCALED (8-bit)");
_Static_assert(VK_FORMAT_R16_UINT - 2 == VK_FORMAT_R16_USCALED && VK_FORMAT_R16G16B16A16_SINT - 2 == VK_FORMAT_R16G16B16A16_SSCALED, "INT-2 = SCALED (16-bit)");
_Static_assert(VK_FORMAT_R8G8B8_UNORM + 14 == VK_FORMAT_R8G8B8A8_UNORM && VK_FORMAT_R8G8B8_SINT + 14 == VK_FORMAT_R8G8B8A8_SINT, "RGB8+14 = RGBA8");
_Static_assert(VK_FORMAT_R16G16B16_UNORM + 7 == VK_FORMAT_R16G16B16A16_UNORM && VK_FORMAT_R16G16B16_SFLOAT + 7 == VK_FORMAT_R16G16B16A16_SFLOAT, "RGB16+7 = RGBA16");
static VkFormat nvmtl_vertex_vkformat(uint32_t mtl, int kind)
{
    static const struct { uint8_t mtl, isint; VkFormat vk; } T[] = {
        { 1, 1, VK_FORMAT_R8G8_UINT }, { 2, 1, VK_FORMAT_R8G8B8_UINT }, { 3, 1, VK_FORMAT_R8G8B8A8_UINT },
        { 4, 1, VK_FORMAT_R8G8_SINT }, { 5, 1, VK_FORMAT_R8G8B8_SINT }, { 6, 1, VK_FORMAT_R8G8B8A8_SINT },
        { 7, 0, VK_FORMAT_R8G8_UNORM }, { 8, 0, VK_FORMAT_R8G8B8_UNORM }, { 9, 0, VK_FORMAT_R8G8B8A8_UNORM },
        { 10, 0, VK_FORMAT_R8G8_SNORM }, { 11, 0, VK_FORMAT_R8G8B8_SNORM }, { 12, 0, VK_FORMAT_R8G8B8A8_SNORM },
        { 13, 1, VK_FORMAT_R16G16_UINT }, { 14, 1, VK_FORMAT_R16G16B16_UINT }, { 15, 1, VK_FORMAT_R16G16B16A16_UINT },
        { 16, 1, VK_FORMAT_R16G16_SINT }, { 17, 1, VK_FORMAT_R16G16B16_SINT }, { 18, 1, VK_FORMAT_R16G16B16A16_SINT },
        { 19, 0, VK_FORMAT_R16G16_UNORM }, { 20, 0, VK_FORMAT_R16G16B16_UNORM }, { 21, 0, VK_FORMAT_R16G16B16A16_UNORM },
        { 22, 0, VK_FORMAT_R16G16_SNORM }, { 23, 0, VK_FORMAT_R16G16B16_SNORM }, { 24, 0, VK_FORMAT_R16G16B16A16_SNORM },
        { 25, 0, VK_FORMAT_R16G16_SFLOAT }, { 26, 0, VK_FORMAT_R16G16B16_SFLOAT }, { 27, 0, VK_FORMAT_R16G16B16A16_SFLOAT },
        { 28, 0, VK_FORMAT_R32_SFLOAT }, { 29, 0, VK_FORMAT_R32G32_SFLOAT }, { 30, 0, VK_FORMAT_R32G32B32_SFLOAT }, { 31, 0, VK_FORMAT_R32G32B32A32_SFLOAT },
        { 32, 2, VK_FORMAT_R32_SINT }, { 33, 2, VK_FORMAT_R32G32_SINT }, { 34, 2, VK_FORMAT_R32G32B32_SINT }, { 35, 2, VK_FORMAT_R32G32B32A32_SINT },
        { 36, 2, VK_FORMAT_R32_UINT }, { 37, 2, VK_FORMAT_R32G32_UINT }, { 38, 2, VK_FORMAT_R32G32B32_UINT }, { 39, 2, VK_FORMAT_R32G32B32A32_UINT },
        { 40, 0, VK_FORMAT_A2B10G10R10_SNORM_PACK32 }, { 41, 0, VK_FORMAT_A2B10G10R10_UNORM_PACK32 }, { 42, 0, VK_FORMAT_B8G8R8A8_UNORM },
        { 45, 1, VK_FORMAT_R8_UINT }, { 46, 1, VK_FORMAT_R8_SINT }, { 47, 0, VK_FORMAT_R8_UNORM }, { 48, 0, VK_FORMAT_R8_SNORM },
        { 49, 1, VK_FORMAT_R16_UINT }, { 50, 1, VK_FORMAT_R16_SINT }, { 51, 0, VK_FORMAT_R16_UNORM }, { 52, 0, VK_FORMAT_R16_SNORM },
        { 53, 0, VK_FORMAT_R16_SFLOAT }, { 54, 0, VK_FORMAT_B10G11R11_UFLOAT_PACK32 }, { 55, 0, VK_FORMAT_E5B9G9R9_UFLOAT_PACK32 } };
    VkFormat v = VK_FORMAT_UNDEFINED; int isint = 0;
    for (size_t i = 0; i < sizeof T / sizeof T[0]; i++) if (T[i].mtl == mtl) { v = T[i].vk; isint = T[i].isint; break; }
    if (v == VK_FORMAT_UNDEFINED) return v;
    if (isint == 1 && kind == 1) v = (VkFormat)(v - 2);
    else if (isint == 2 && kind == 1) { static int said; if (said++ < 4) nvlog("G16 vertex format %u: a 32-bit integer read as float has no Vulkan format — bound as integer, values will be WRONG", mtl); }
    if (!nvmtl_vk_format_is_vertex(v)) {
        VkFormat wide = (v >= VK_FORMAT_R8G8B8_UNORM && v <= VK_FORMAT_R8G8B8_SINT) ? (VkFormat)(v + 14)
                      : (v >= VK_FORMAT_R16G16B16_UNORM && v <= VK_FORMAT_R16G16B16_SFLOAT) ? (VkFormat)(v + 7) : VK_FORMAT_UNDEFINED;
        if (wide != VK_FORMAT_UNDEFINED && nvmtl_vk_format_is_vertex(wide)) return wide;
        nvlog("G16 vertex format %u -> VkFormat %d is not a vertex-buffer format on this device", mtl, (int)v); return VK_FORMAT_UNDEFINED;
    }
    return v;
}
static int nvmtl_vertex_input_build(const nvk_vertex_input *vin, const void *vs, size_t vn, VkVertexInputBindingDescription *vib, uint32_t *nvib,
                                    VkVertexInputAttributeDescription *via, uint32_t *nvia, uint32_t *mask, uint32_t *dyn, uint32_t stv[32])
{
    *nvib = *nvia = *mask = *dyn = 0; memset(stv, 0, 32 * sizeof *stv);
    uint8_t kind[32]; nvmtl_spv_input_kinds(vs, vn, kind);
    uint32_t described = 0;
    for (uint32_t k = 0; vin && k < vin->nattr && k < NVMTL_NVIN; k++) {
        uint32_t loc = vin->attr[k].location, buf = vin->attr[k].buffer;
        if (loc >= NVMTL_NVIN || buf >= NVMTL_NVIN) { nvlog("G16 pipeline: vertex attribute %u / buffer %u is outside [0,%d)", loc, buf, NVMTL_NVIN); return -1; }
        described |= 1u << loc;
        if (!kind[loc]) continue;
        VkFormat f = nvmtl_vertex_vkformat(vin->attr[k].mtlfmt, kind[loc]);
        if (f == VK_FORMAT_UNDEFINED) { nvlog("G16 pipeline: MTLVertexFormat %u at attribute %u is not carried", vin->attr[k].mtlfmt, loc); return -1; }
        via[*nvia] = (VkVertexInputAttributeDescription){ loc, buf, f, vin->attr[k].offset }; (*nvia)++;
        if (*mask & (1u << buf)) continue;
        *mask |= 1u << buf;
        uint32_t step = vin->layout[buf].step, stride = step == 0 ? 0 : vin->layout[buf].stride;
        if (stride == NVMTL_STRIDE_DYNAMIC) { *dyn |= 1u << buf; stride = 0; }
        stv[buf] = stride;
        if (step > 2) { nvlog("G16 pipeline: vertex step function %u (per patch) is tessellation — not carried", step); return -1; }
        if (step == 2 && vin->layout[buf].rate != 1) { static int said; if (said++ < 4) nvlog("G16 pipeline: per-instance step rate %u needs "
            "VK_EXT_vertex_attribute_divisor, which is not wired — stepping once per instance", vin->layout[buf].rate); }
        vib[*nvib] = (VkVertexInputBindingDescription){ buf, stride, step == 2 ? VK_VERTEX_INPUT_RATE_INSTANCE : VK_VERTEX_INPUT_RATE_VERTEX }; (*nvib)++;
    }
    for (uint32_t l = 0; l < NVMTL_NVIN; l++) if (kind[l] && !(described & (1u << l))) { static int said; if (said++ < 8)
        nvlog("G16 pipeline: the vertex shader reads attribute %u and the vertex descriptor does not describe it — it reads undefined values", l); }
    return 0;
}
static uint32_t nvmtl_rt_shape(const nvk_pipeline *p, uint32_t cf[NVMTL_NCOL], uint32_t *rmask)
{
    memset(cf, 0, NVMTL_NCOL * sizeof *cf);
    *rmask = p->rmask;
    if (!p->ncol && p->cfmt) { cf[0] = p->cfmt; *rmask = p->samples > 1 ? 1u : 0u; return 1; }
    uint32_t n = p->ncol > NVMTL_NCOL ? NVMTL_NCOL : p->ncol;
    for (uint32_t i = 0; i < n; i++) cf[i] = p->cfmts[i];
    return n;
}
static VkResult nvmtl_make_render_pass(const nvk_pipeline *p, uint32_t load, VkRenderPass *out)
{
    VkSampleCountFlagBits sc = (VkSampleCountFlagBits)(p->samples > 1 ? p->samples : 1);
    int ms = p->samples > 1, anyres = 0;
    uint32_t cf[NVMTL_NCOL], rmask, ncol = nvmtl_rt_shape(p, cf, &rmask);
    VkAttachmentDescription atts[2 * NVMTL_NCOL + 1]; uint32_t n = 0;
    VkAttachmentReference cref[NVMTL_NCOL], rref[NVMTL_NCOL], dref = { 0, VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL };
    for (uint32_t i = 0; i < ncol; i++) {
        cref[i] = rref[i] = (VkAttachmentReference){ VK_ATTACHMENT_UNUSED, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL };
        if (!cf[i]) continue;
        int ld = (load & NVMTL_LOAD_COLOUR(i)) != 0;
        cref[i].attachment = n;
        atts[n++] = (VkAttachmentDescription){ 0, (VkFormat)cf[i], sc,
            ld ? VK_ATTACHMENT_LOAD_OP_LOAD : VK_ATTACHMENT_LOAD_OP_CLEAR, VK_ATTACHMENT_STORE_OP_STORE,
            VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_DONT_CARE,
            ld ? VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL : VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL };
    }
    if (p->has_depth) {
        int dl = (load & NVMTL_LOAD_DEPTH) != 0, sl = p->has_stencil ? (load & NVMTL_LOAD_STENCIL) != 0 : dl;
        dref.attachment = n;
        atts[n++] = (VkAttachmentDescription){ 0, (VkFormat)(p->dfmt ? p->dfmt : VK_FORMAT_D32_SFLOAT), sc,
            dl ? VK_ATTACHMENT_LOAD_OP_LOAD : VK_ATTACHMENT_LOAD_OP_CLEAR, VK_ATTACHMENT_STORE_OP_STORE,
            sl ? VK_ATTACHMENT_LOAD_OP_LOAD : p->has_stencil ? VK_ATTACHMENT_LOAD_OP_CLEAR : VK_ATTACHMENT_LOAD_OP_DONT_CARE,
            (sl || p->has_stencil) ? VK_ATTACHMENT_STORE_OP_STORE : VK_ATTACHMENT_STORE_OP_DONT_CARE,
            (dl || sl) ? VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL : VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL };
    }
    for (uint32_t i = 0; ms && i < ncol; i++) {
        if (!cf[i] || !(rmask & (1u << i))) continue;
        rref[i].attachment = n; anyres = 1;
        atts[n++] = (VkAttachmentDescription){ 0, (VkFormat)cf[i], VK_SAMPLE_COUNT_1_BIT,
            VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_STORE, VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_DONT_CARE,
            VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL };
    }
    VkSubpassDescription sp = { 0, VK_PIPELINE_BIND_POINT_GRAPHICS, 0, NULL, ncol, ncol ? cref : NULL, anyres ? rref : NULL,
        p->has_depth ? &dref : NULL, 0, NULL };
    const VkSubpassDependency deps[2] = {
        { VK_SUBPASS_EXTERNAL, 0, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
          VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT, 0 },
        { 0, VK_SUBPASS_EXTERNAL, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
          VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT, 0 } };
    VkRenderPassCreateInfo rpci = { VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO, NULL, 0, n, atts, 1, &sp, 2, deps };
    return pvkCreateRenderPass(g_dev, &rpci, NULL, out);
}
int nvmtl_vk_pipeline_create_blend(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, const nvk_blend_state *blend, nvk_pipeline *out)
{ return nvmtl_vk_pipeline_create_vin(vs, vn, fs, fn, vkfmt, depthfmt, blend, NULL, out); }
int nvmtl_vk_pipeline_create_vin(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, const nvk_blend_state *blend,
                                 const nvk_vertex_input *vin, nvk_pipeline *out)
{ return nvmtl_vk_pipeline_create_vin_ms(vs, vn, fs, fn, vkfmt, depthfmt, blend, vin, 1, out); }
#define NVMTL_TESS_NVB 8
#define NVMTL_TESS_PC_BYTES 96
_Static_assert(NVMTL_SAMPLE_POSITION_OFFSET >= NVMTL_TESS_PC_BYTES, "fragment and TESC push ranges must not overlap");

typedef struct { uint32_t *w; size_t n, cap; int bad; } nvtsb;
static void nvt_put(nvtsb *b, uint32_t v) {
    if (b->bad) return;
    if (b->n == b->cap) { size_t nc = b->cap ? b->cap * 2 : 256; uint32_t *nw = (uint32_t *)realloc(b->w, nc * 4);
        if (!nw) { b->bad = 1; return; } b->w = nw; b->cap = nc; }
    b->w[b->n++] = v;
}
static void nvt_ins(nvtsb *b, uint32_t op, const uint32_t *a, uint32_t na) { nvt_put(b, ((na + 1) << 16) | op); for (uint32_t i = 0; i < na; i++) nvt_put(b, a[i]); }
#define NVT(b, op, ...) do { const uint32_t a_[] = { __VA_ARGS__ }; nvt_ins((b), (op), a_, (uint32_t)(sizeof a_ / sizeof a_[0])); } while (0)
static void nvt_str(nvtsb *b, uint32_t op, const uint32_t *pre, uint32_t npre, const char *s, const uint32_t *post, uint32_t npost) {
    size_t sl = strlen(s), sw = sl / 4 + 1;
    nvt_put(b, (uint32_t)((1 + npre + sw + npost) << 16) | op);
    for (uint32_t i = 0; i < npre; i++) nvt_put(b, pre[i]);
    for (size_t i = 0; i < sw; i++) { uint32_t v = 0; for (int k = 0; k < 4; k++) { size_t j = i * 4 + k; if (j < sl) v |= (uint32_t)(uint8_t)s[j] << (8 * k); } nvt_put(b, v); }
    for (uint32_t i = 0; i < npost; i++) nvt_put(b, post[i]);
}

typedef struct {
    nvtsb caps, ep, deco, glob, fn;
    uint32_t id, glsl, cur;
    uint32_t t_void, t_fn, t_bool, t_u64, t_vec[3][5];
    struct { uint32_t ty, lo, hi, id; } k[96]; int nk;
    struct { uint32_t sc, ty, id; } p[48]; int np;
    int err;
} nvtc;
static uint32_t nvt_id(nvtc *c) { return c->id++; }
static uint32_t nvt_ptr(nvtc *c, uint32_t sc, uint32_t ty) {
    for (int i = 0; i < c->np; i++) if (c->p[i].sc == sc && c->p[i].ty == ty) return c->p[i].id;
    if (c->np == 48) { c->err = 1; return 0; }
    uint32_t id = nvt_id(c); NVT(&c->glob, 32, id, sc, ty); c->p[c->np].sc = sc; c->p[c->np].ty = ty; c->p[c->np++].id = id; return id;
}
static uint32_t nvt_k2(nvtc *c, uint32_t ty, uint32_t lo, uint32_t hi, int wide) {
    for (int i = 0; i < c->nk; i++) if (c->k[i].ty == ty && c->k[i].lo == lo && c->k[i].hi == hi) return c->k[i].id;
    if (c->nk == 96) { c->err = 1; return 0; }
    uint32_t id = nvt_id(c);
    if (wide) NVT(&c->glob, 43, ty, id, lo, hi); else NVT(&c->glob, 43, ty, id, lo);
    c->k[c->nk].ty = ty; c->k[c->nk].lo = lo; c->k[c->nk].hi = hi; c->k[c->nk++].id = id; return id;
}
#define NVT_F 0
#define NVT_I 1
#define NVT_U 2
static uint32_t nvt_ku(nvtc *c, uint32_t v) { return nvt_k2(c, c->t_vec[NVT_U][1], v, 0, 0); }
static uint32_t nvt_ki(nvtc *c, int32_t v) { return nvt_k2(c, c->t_vec[NVT_I][1], (uint32_t)v, 0, 0); }
static uint32_t nvt_kf(nvtc *c, float f) { uint32_t v; memcpy(&v, &f, 4); return nvt_k2(c, c->t_vec[NVT_F][1], v, 0, 0); }
static uint32_t nvt_k64(nvtc *c, uint64_t v) { return nvt_k2(c, c->t_u64, (uint32_t)v, (uint32_t)(v >> 32), 1); }
static uint32_t nvt_op1(nvtc *c, uint32_t op, uint32_t ty, uint32_t a) { uint32_t r = nvt_id(c); NVT(&c->fn, op, ty, r, a); return r; }
static uint32_t nvt_op2(nvtc *c, uint32_t op, uint32_t ty, uint32_t a, uint32_t b) { uint32_t r = nvt_id(c); NVT(&c->fn, op, ty, r, a, b); return r; }
static uint32_t nvt_ext1(nvtc *c, uint32_t ty, uint32_t inst, uint32_t a) { uint32_t r = nvt_id(c); NVT(&c->fn, 12, ty, r, c->glsl, inst, a); return r; }
static uint32_t nvt_ext2(nvtc *c, uint32_t ty, uint32_t inst, uint32_t a, uint32_t b) { uint32_t r = nvt_id(c); NVT(&c->fn, 12, ty, r, c->glsl, inst, a, b); return r; }
static uint32_t nvt_sel(nvtc *c, uint32_t ty, uint32_t cond, uint32_t a, uint32_t b) { uint32_t r = nvt_id(c); NVT(&c->fn, 169, ty, r, cond, a, b); return r; }
static uint32_t nvt_ex(nvtc *c, uint32_t ty, uint32_t comp, uint32_t i) { uint32_t r = nvt_id(c); NVT(&c->fn, 81, ty, r, comp, i); return r; }
static uint32_t nvt_add64(nvtc *c, uint32_t a, uint32_t b) { return nvt_op2(c, 128, c->t_u64, a, b); }
static uint32_t nvt_widen(nvtc *c, uint32_t u32) { return nvt_op1(c, 113, c->t_u64, u32); }
static uint32_t nvt_load32(nvtc *c, uint32_t addr) {
    uint32_t pt = nvt_ptr(c, 5349, c->t_vec[NVT_U][1]), p = nvt_op1(c, 120, pt, addr), v = nvt_id(c);
    NVT(&c->fn, 61, c->t_vec[NVT_U][1], v, p, 2, 4);
    return v;
}
static void nvt_types(nvtc *c, int wide) {
    c->id = 1;
    c->t_void = nvt_id(c); NVT(&c->glob, 19, c->t_void);
    c->t_fn = nvt_id(c); NVT(&c->glob, 33, c->t_fn, c->t_void);
    c->t_bool = nvt_id(c); NVT(&c->glob, 20, c->t_bool);
    c->t_vec[NVT_F][1] = nvt_id(c); NVT(&c->glob, 22, c->t_vec[NVT_F][1], 32);
    c->t_vec[NVT_I][1] = nvt_id(c); NVT(&c->glob, 21, c->t_vec[NVT_I][1], 32, 1);
    c->t_vec[NVT_U][1] = nvt_id(c); NVT(&c->glob, 21, c->t_vec[NVT_U][1], 32, 0);
    if (wide) { c->t_u64 = nvt_id(c); NVT(&c->glob, 21, c->t_u64, 64, 0); }
    for (int k = 0; k < 3; k++) for (uint32_t n = 2; n <= 4; n++) { c->t_vec[k][n] = nvt_id(c); NVT(&c->glob, 23, c->t_vec[k][n], c->t_vec[k][1], n); }
}
static uint32_t *nvt_link(nvtc *c, size_t *outn) {
    nvtsb *s[5] = { &c->caps, &c->ep, &c->deco, &c->glob, &c->fn };
    size_t n = 5; for (int i = 0; i < 5; i++) { if (s[i]->bad) c->err = 1; n += s[i]->n; }
    uint32_t *w = c->err ? NULL : (uint32_t *)malloc(n * 4);
    if (w) {
        w[0] = 0x07230203; w[1] = 0x00010300; w[2] = 0; w[3] = c->id; w[4] = 0;
        size_t o = 5; for (int i = 0; i < 5; i++) { if (s[i]->n) memcpy(w + o, s[i]->w, s[i]->n * 4); o += s[i]->n; }
        *outn = n;
    }
    for (int i = 0; i < 5; i++) free(s[i]->w);
    return w;
}

typedef struct { uint32_t op, a, b; } nvt_def;
typedef struct { uint32_t var, loc, patch, kind, comps, arrlen; } nvt_in;
typedef struct { uint32_t model, domain, spacing_at, order_at, pid_deco_at, pid_var, pid_kind, maxloc; int has_loc; int nin; nvt_in in[40]; } nvt_tese;
static int nvt_scan_tese(const uint32_t *w, size_t n, nvt_tese *t) {
    memset(t, 0, sizeof *t); t->model = ~0u;
    if (n < 5 || w[0] != 0x07230203) return -1;
    uint32_t bound = w[3]; if (!bound || bound > (1u << 22)) return -1;
    nvt_def *d = (nvt_def *)calloc(bound, sizeof *d); uint32_t *loc = (uint32_t *)calloc(bound, 4), *fl = (uint32_t *)calloc(bound, 4);
    if (!d || !loc || !fl) { free(d); free(loc); free(fl); return -1; }
    int rc = 0;
    for (size_t i = 5; i < n; ) {
        uint32_t wc = w[i] >> 16, op = w[i] & 0xffff;
        if (!wc || i + wc > n) { rc = -1; break; }
        const uint32_t *o = w + i + 1;
        #define NVT_ID_OK(x) ((x) < bound)
        if (op == 15 && wc >= 3) t->model = o[0];
        else if (op == 16 && wc >= 3) {
            if (o[1] == 22 || o[1] == 24 || o[1] == 25) t->domain = o[1];
            else if (o[1] >= 1 && o[1] <= 3) t->spacing_at = (uint32_t)(i + 2);
            else if (o[1] == 4 || o[1] == 5) t->order_at = (uint32_t)(i + 2);
        } else if (op == 71 && wc >= 3 && NVT_ID_OK(o[0])) {
            if (o[1] == 30 && wc >= 4) loc[o[0]] = o[2] + 1;
            else if (o[1] == 15) fl[o[0]] |= 1;
            else if (o[1] == 11 && wc >= 4) { fl[o[0]] |= 2; if (o[2] == 7) { t->pid_deco_at = (uint32_t)i; t->pid_var = o[0]; } }
        } else if ((op == 21 || op == 22 || op == 23 || op == 28 || op == 32) && wc >= 3 && NVT_ID_OK(o[0])) {
            d[o[0]].op = op; d[o[0]].a = o[1]; d[o[0]].b = wc >= 4 ? o[2] : 0;
        } else if (op == 43 && wc >= 4 && NVT_ID_OK(o[1])) { d[o[1]].op = 43; d[o[1]].a = o[0]; d[o[1]].b = o[2]; }
        else if (op == 59 && wc >= 4 && NVT_ID_OK(o[1]) && o[2] == 1) { d[o[1]].op = 59; d[o[1]].a = o[0]; }
        i += wc;
    }
    for (uint32_t id = 0; rc == 0 && id < bound; id++) {
        if (d[id].op != 59 || !loc[id] || (fl[id] & 2)) continue;
        uint32_t L = loc[id] - 1; if (!t->has_loc || L > t->maxloc) t->maxloc = L; t->has_loc = 1;
        if (t->nin == 40) { rc = -2; break; }
        nvt_in *e = &t->in[t->nin++]; e->var = id; e->loc = L; e->patch = fl[id] & 1;
        uint32_t ty = d[id].a < bound && d[d[id].a].op == 32 ? d[d[id].a].b : 0;
        if (ty < bound && d[ty].op == 28) {
            uint32_t len = d[ty].b; e->arrlen = len < bound && d[len].op == 43 ? d[len].b : 0; ty = d[ty].a;
        }
        e->comps = 1;
        if (ty < bound && d[ty].op == 23) { e->comps = d[ty].b; ty = d[ty].a; }
        if (ty < bound && d[ty].op == 22 && d[ty].a == 32) e->kind = NVT_F;
        else if (ty < bound && d[ty].op == 21 && d[ty].a == 32) e->kind = d[ty].b ? NVT_I : NVT_U;
        else e->kind = 99;
    }
    if (rc == 0 && t->pid_var) {
        uint32_t pt = t->pid_var < bound ? d[t->pid_var].a : 0, ty = pt < bound && d[pt].op == 32 ? d[pt].b : 0;
        t->pid_kind = ty < bound && d[ty].op == 21 && d[ty].a == 32 ? (d[ty].b ? NVT_I : NVT_U) : 99;
    }
    #undef NVT_ID_OK
    free(d); free(loc); free(fl);
    return rc;
}
int nvmtl_spirv_tess_info(const void *spv, size_t bytes, uint32_t *patchType, uint32_t *cps) {
    nvt_tese t; if (!spv || nvt_scan_tese((const uint32_t *)spv, bytes / 4, &t) || t.model != 2) return 0;
    *patchType = t.domain == 22 ? 1 : t.domain == 24 ? 2 : 0; *cps = 0;
    for (int i = 0; i < t.nin; i++) if (!t.in[i].patch && t.in[i].arrlen) { *cps = t.in[i].arrlen; break; }
    return *patchType != 0;
}

static uint32_t *nvmtl_tess_patch_tese(const uint32_t *w, size_t n, uint32_t spacing, uint32_t order, size_t *outn, uint32_t *pidloc, const char **why) {
    nvt_tese t; *pidloc = ~0u;
    if (nvt_scan_tese(w, n, &t)) { *why = "the post-tessellation module does not parse"; return NULL; }
    if (t.model != 2) { *why = "the vertex function is not a post-tessellation (TessellationEvaluation) module"; return NULL; }
    if (!t.spacing_at || !t.order_at) { *why = "the post-tessellation module declares no spacing or vertex order to set"; return NULL; }
    uint32_t *o = (uint32_t *)malloc((n + 3) * 4); if (!o) { *why = "out of memory"; return NULL; }
    size_t at = t.pid_deco_at;
    if (!at) { memcpy(o, w, n * 4); *outn = n; }
    else {
        memcpy(o, w, (at + 4) * 4);
        *pidloc = t.has_loc ? t.maxloc + 1 : 0;
        o[at + 2] = 30; o[at + 3] = *pidloc;
        o[at + 4] = (3u << 16) | 71; o[at + 5] = t.pid_var; o[at + 6] = 15;
        memcpy(o + at + 7, w + at + 4, (n - at - 4) * 4); *outn = n + 3;
    }
    size_t sh = at && t.spacing_at > at ? 3 : 0, oh = at && t.order_at > at ? 3 : 0;
    o[t.spacing_at + sh] = spacing; o[t.order_at + oh] = order;
    return o;
}

typedef struct { uint8_t fmt, kind, bits, comps, bgra; } nvt_fmt;
static const nvt_fmt k_nvt_fmt[] = {
    { 1, 4, 8, 2, 0 }, { 2, 4, 8, 3, 0 }, { 3, 4, 8, 4, 0 }, { 4, 5, 8, 2, 0 }, { 5, 5, 8, 3, 0 }, { 6, 5, 8, 4, 0 },
    { 7, 2, 8, 2, 0 }, { 8, 2, 8, 3, 0 }, { 9, 2, 8, 4, 0 }, { 10, 3, 8, 2, 0 }, { 11, 3, 8, 3, 0 }, { 12, 3, 8, 4, 0 },
    { 13, 4, 16, 2, 0 }, { 14, 4, 16, 3, 0 }, { 15, 4, 16, 4, 0 }, { 16, 5, 16, 2, 0 }, { 17, 5, 16, 3, 0 }, { 18, 5, 16, 4, 0 },
    { 19, 2, 16, 2, 0 }, { 20, 2, 16, 3, 0 }, { 21, 2, 16, 4, 0 }, { 22, 3, 16, 2, 0 }, { 23, 3, 16, 3, 0 }, { 24, 3, 16, 4, 0 },
    { 25, 1, 16, 2, 0 }, { 26, 1, 16, 3, 0 }, { 27, 1, 16, 4, 0 }, { 28, 0, 32, 1, 0 }, { 29, 0, 32, 2, 0 }, { 30, 0, 32, 3, 0 },
    { 31, 0, 32, 4, 0 }, { 32, 5, 32, 1, 0 }, { 33, 5, 32, 2, 0 }, { 34, 5, 32, 3, 0 }, { 35, 5, 32, 4, 0 }, { 36, 4, 32, 1, 0 },
    { 37, 4, 32, 2, 0 }, { 38, 4, 32, 3, 0 }, { 39, 4, 32, 4, 0 }, { 42, 2, 8, 4, 1 }, { 45, 4, 8, 1, 0 }, { 46, 5, 8, 1, 0 },
    { 47, 2, 8, 1, 0 }, { 48, 3, 8, 1, 0 }, { 49, 4, 16, 1, 0 }, { 50, 5, 16, 1, 0 }, { 51, 2, 16, 1, 0 }, { 52, 3, 16, 1, 0 },
    { 53, 1, 16, 1, 0 },
};
static const nvt_fmt *nvt_fmt_of(uint32_t f) { for (size_t i = 0; i < sizeof k_nvt_fmt / sizeof k_nvt_fmt[0]; i++) if (k_nvt_fmt[i].fmt == f) return &k_nvt_fmt[i]; return NULL; }
static int nvt_fmt_class(const nvt_fmt *f) { return f->kind <= 3 ? NVT_F : f->kind == 4 ? NVT_U : NVT_I; }

static uint32_t nvt_pull(nvtc *c, const nvt_fmt *f, uint32_t addr, uint32_t kind, uint32_t m) {
    uint32_t wd[4] = { 0 }, comp[4] = { 0 }, nw = (f->comps * f->bits / 8 + 3) / 4, F = c->t_vec[NVT_F][1], U = c->t_vec[NVT_U][1], I = c->t_vec[NVT_I][1];
    for (uint32_t i = 0; i < nw; i++) wd[i] = nvt_load32(c, i ? nvt_add64(c, addr, nvt_k64(c, 4 * i)) : addr);
    for (uint32_t k = 0; k < f->comps; k++) {
        uint32_t word = wd[k * f->bits / 32], off = (k * f->bits) % 32;
        float scale = f->kind == 2 ? (float)((1u << f->bits) - 1) : f->kind == 3 ? (float)((1u << (f->bits - 1)) - 1) : 1.0f;
        switch (f->kind) {
        case 0: comp[k] = nvt_op1(c, 124, F, word); break;
        case 1: comp[k] = nvt_ex(c, F, nvt_ext1(c, c->t_vec[NVT_F][2], 62, word), off / 16); break;
        case 2: comp[k] = nvt_op2(c, 133, F, nvt_op1(c, 112, F, f->bits == 32 ? word :
                    ({ uint32_t r = nvt_id(c); NVT(&c->fn, 203, U, r, word, nvt_ku(c, off), nvt_ku(c, f->bits)); r; })), nvt_kf(c, 1.0f / scale)); break;
        case 3: { uint32_t s = nvt_id(c); NVT(&c->fn, 202, I, s, nvt_op1(c, 124, I, word), nvt_ku(c, off), nvt_ku(c, f->bits));
                  comp[k] = nvt_ext2(c, F, 40, nvt_op2(c, 133, F, nvt_op1(c, 111, F, s), nvt_kf(c, 1.0f / scale)), nvt_kf(c, -1.0f)); break; }
        case 4: if (f->bits == 32) comp[k] = word; else { comp[k] = nvt_id(c); NVT(&c->fn, 203, U, comp[k], word, nvt_ku(c, off), nvt_ku(c, f->bits)); } break;
        default: { uint32_t iw = nvt_op1(c, 124, I, word); if (f->bits == 32) comp[k] = iw;
                   else { comp[k] = nvt_id(c); NVT(&c->fn, 202, I, comp[k], iw, nvt_ku(c, off), nvt_ku(c, f->bits)); } break; }
        }
    }
    if (f->bgra) { uint32_t s = comp[0]; comp[0] = comp[2]; comp[2] = s; }
    uint32_t have = (uint32_t)nvt_fmt_class(f), v[4];
    for (uint32_t j = 0; j < m; j++) {
        if (j < f->comps) v[j] = have == kind ? comp[j] : nvt_op1(c, 124, c->t_vec[kind][1], comp[j]);
        else v[j] = kind == NVT_F ? nvt_kf(c, j == 3 ? 1.0f : 0.0f) : kind == NVT_I ? nvt_ki(c, j == 3) : nvt_ku(c, j == 3);
    }
    if (m == 1) return v[0];
    uint32_t r = nvt_id(c);
    if (m == 2) NVT(&c->fn, 80, c->t_vec[kind][2], r, v[0], v[1]);
    else if (m == 3) NVT(&c->fn, 80, c->t_vec[kind][3], r, v[0], v[1], v[2]);
    else NVT(&c->fn, 80, c->t_vec[kind][4], r, v[0], v[1], v[2], v[3]);
    return r;
}
static uint32_t nvt_load_if(nvtc *c, uint32_t cond, uint32_t addr, uint32_t other) {
    uint32_t lt = nvt_id(c), lm = nvt_id(c), from = c->cur;
    NVT(&c->fn, 247, lm, 0); NVT(&c->fn, 250, cond, lt, lm);
    NVT(&c->fn, 248, lt); c->cur = lt; uint32_t v = nvt_load32(c, addr); NVT(&c->fn, 249, lm);
    NVT(&c->fn, 248, lm); uint32_t r = nvt_id(c); NVT(&c->fn, 245, c->t_vec[NVT_U][1], r, v, lt, other, from); c->cur = lm;
    return r;
}

#ifndef NVMTL_TESS_DESC_DEFINED
#define NVMTL_TESS_DESC_DEFINED
typedef struct { uint32_t patch_type, n, partition, winding, stepfn, scale_en, cpidx; float maxf; int32_t inst_loc; } nvk_tess_desc;
#endif

static uint32_t *nvmtl_tess_make_vs(size_t *n) {
    nvtc c; memset(&c, 0, sizeof c); nvt_types(&c, 0);
    NVT(&c.caps, 17, 1); NVT(&c.caps, 14, 0, 1);
    uint32_t I = c.t_vec[NVT_I][1], U2 = c.t_vec[NVT_U][2];
    uint32_t pin = nvt_ptr(&c, 1, I), pout = nvt_ptr(&c, 3, U2);
    uint32_t vi = nvt_id(&c), ii = nvt_id(&c), out = nvt_id(&c), main_ = nvt_id(&c);
    NVT(&c.glob, 59, pin, vi, 1); NVT(&c.glob, 59, pin, ii, 1); NVT(&c.glob, 59, pout, out, 3);
    NVT(&c.deco, 71, vi, 11, 42); NVT(&c.deco, 71, ii, 11, 43); NVT(&c.deco, 71, out, 30, 0);
    { const uint32_t pre[2] = { 0, main_ }, post[3] = { vi, ii, out }; nvt_str(&c.ep, 15, pre, 2, "main", post, 3); }
    NVT(&c.fn, 54, c.t_void, main_, 0, c.t_fn); NVT(&c.fn, 248, nvt_id(&c));
    uint32_t a = nvt_id(&c), b = nvt_id(&c); NVT(&c.fn, 61, I, a, vi); NVT(&c.fn, 61, I, b, ii);
    uint32_t v = nvt_id(&c); NVT(&c.fn, 80, U2, v, nvt_op1(&c, 124, c.t_vec[NVT_U][1], a), nvt_op1(&c, 124, c.t_vec[NVT_U][1], b));
    NVT(&c.fn, 62, out, v); NVT(&c.fn, 253); NVT(&c.fn, 56);
    return nvt_link(&c, n);
}

static uint32_t *nvmtl_tess_make_tcs(const nvt_tese *t, const nvk_tess_desc *td, const nvk_vertex_input *vin, uint32_t pidloc,
                                     uint32_t *vbmap, uint32_t *nvb, size_t *n, char *why, size_t wl) {
    nvtc c; memset(&c, 0, sizeof c); nvt_types(&c, 1);
    uint32_t F = c.t_vec[NVT_F][1], U = c.t_vec[NVT_U][1], I = c.t_vec[NVT_I][1], U64 = c.t_u64, quad = td->patch_type == 2;
    *nvb = 0;
    struct { const nvt_in *in; const nvt_fmt *f; uint32_t off, slot, stride, step; int sys; } pl[40]; int npl = 0;
    int inst_found = 0;
    for (int i = 0; i < t->nin; i++) {
        const nvt_in *e = &t->in[i];
        if (e->loc == pidloc && e->patch) continue;
        if (e->kind == 99) { snprintf(why, wl, "TESE input Location %u is not a 32-bit scalar/vector", e->loc); goto refuse; }
        if (!e->patch && e->arrlen != td->n) { snprintf(why, wl, "TESE control-point input Location %u has %u elements, the patch has %u", e->loc, e->arrlen, td->n); goto refuse; }
        int a = -1; for (uint32_t k = 0; vin && k < vin->nattr; k++) if (vin->attr[k].location == e->loc) a = (int)k;
        int is_inst = e->patch && ((td->inst_loc >= 0 && (uint32_t)td->inst_loc == e->loc) ||
                                   (td->inst_loc < 0 && a < 0 && !inst_found && e->comps == 1 && e->kind != NVT_F));
        if (is_inst) { if (e->comps != 1 || e->kind == NVT_F) { snprintf(why, wl, "instance_id Location %u is not an integer scalar", e->loc); goto refuse; }
            pl[npl].in = e; pl[npl++].sys = 1; inst_found = 1; continue; }
        if (a < 0) { snprintf(why, wl, "TESE reads Location %u and the vertex descriptor has no attribute %u", e->loc, e->loc); goto refuse; }
        const nvt_fmt *f = nvt_fmt_of(vin->attr[a].mtlfmt);
        if (!f) { snprintf(why, wl, "attribute %u: MTLVertexFormat %u is not carried for tessellation", e->loc, vin->attr[a].mtlfmt); goto refuse; }
        if ((nvt_fmt_class(f) == NVT_F) != (e->kind == NVT_F)) { snprintf(why, wl, "attribute %u: format %u and the shader's type disagree on float vs integer", e->loc, vin->attr[a].mtlfmt); goto refuse; }
        uint32_t b = vin->attr[a].buffer, step = vin->layout[b].step, rate = vin->layout[b].rate;
        if (step == 1) { snprintf(why, wl, "attribute %u: buffer %u steps PER VERTEX, which a post-tessellation function cannot fetch", e->loc, b); goto refuse; }
        if (step == 2 && rate != 1) { snprintf(why, wl, "attribute %u: per-instance step rate %u is not carried for tessellation", e->loc, rate); goto refuse; }
        if (step && vin->layout[b].stride == NVMTL_STRIDE_DYNAMIC) { snprintf(why, wl, "attribute %u: buffer %u has a dynamic stride "
            "(MTLBufferLayoutStrideDynamic), which the tessellation pull does not carry", e->loc, b); goto refuse; }
        if (vin->attr[a].offset & 3 || (step && vin->layout[b].stride & 3)) { snprintf(why, wl, "attribute %u: offset/stride not 4-byte aligned", e->loc); goto refuse; }
        uint32_t s = 0; while (s < *nvb && vbmap[s] != b) s++;
        if (s == *nvb) { if (*nvb == NVMTL_TESS_NVB) { snprintf(why, wl, "more than %d vertex buffers feed one tessellation pipeline", NVMTL_TESS_NVB); goto refuse; } vbmap[(*nvb)++] = b; }
        pl[npl].in = e; pl[npl].f = f; pl[npl].off = vin->attr[a].offset; pl[npl].slot = s; pl[npl].stride = step ? vin->layout[b].stride : 0;
        pl[npl].step = step; pl[npl++].sys = 0;
    }

    NVT(&c.caps, 17, 1); NVT(&c.caps, 17, 3); NVT(&c.caps, 17, 11); NVT(&c.caps, 17, 5347);
    { const uint32_t none[1] = { 0 }; nvt_str(&c.caps, 10, none, 0, "SPV_KHR_physical_storage_buffer", none, 0);
      c.glsl = nvt_id(&c); const uint32_t pre[1] = { c.glsl }; nvt_str(&c.caps, 11, pre, 1, "GLSL.std.450", none, 0); }
    NVT(&c.caps, 14, 5348, 1);
    uint32_t arr8 = nvt_id(&c); NVT(&c.glob, 28, arr8, U64, nvt_ku(&c, NVMTL_TESS_NVB)); NVT(&c.deco, 71, arr8, 6, 8);
    uint32_t pcs = nvt_id(&c); NVT(&c.glob, 30, pcs, U64, U64, U64, U, F, arr8);
    NVT(&c.deco, 71, pcs, 2);
    { const uint32_t offs[6] = { 0, 8, 16, 24, 28, 32 }; for (uint32_t m = 0; m < 6; m++) NVT(&c.deco, 72, pcs, m, 35, offs[m]); }
    uint32_t pc = nvt_id(&c); NVT(&c.glob, 59, nvt_ptr(&c, 9, pcs), pc, 9);
    uint32_t inids_t = nvt_id(&c); NVT(&c.glob, 28, inids_t, c.t_vec[NVT_U][2], nvt_ku(&c, 32));
    uint32_t inids = nvt_id(&c); NVT(&c.glob, 59, nvt_ptr(&c, 1, inids_t), inids, 1); NVT(&c.deco, 71, inids, 30, 0);
    uint32_t invid = nvt_id(&c); NVT(&c.glob, 59, nvt_ptr(&c, 1, I), invid, 1); NVT(&c.deco, 71, invid, 11, 8);
    uint32_t f4 = nvt_id(&c); NVT(&c.glob, 28, f4, F, nvt_ku(&c, 4)); uint32_t f2 = nvt_id(&c); NVT(&c.glob, 28, f2, F, nvt_ku(&c, 2));
    uint32_t outer = nvt_id(&c); NVT(&c.glob, 59, nvt_ptr(&c, 3, f4), outer, 3); NVT(&c.deco, 71, outer, 11, 11); NVT(&c.deco, 71, outer, 15);
    uint32_t inner = nvt_id(&c); NVT(&c.glob, 59, nvt_ptr(&c, 3, f2), inner, 3); NVT(&c.deco, 71, inner, 11, 12); NVT(&c.deco, 71, inner, 15);
    uint32_t iface[48]; uint32_t nif = 0; iface[nif++] = inids; iface[nif++] = invid; iface[nif++] = outer; iface[nif++] = inner;
    uint32_t outv[40], pidout = 0;
    for (int i = 0; i < npl; i++) {
        const nvt_in *e = pl[i].in; uint32_t ty = c.t_vec[e->kind][e->comps];
        if (!e->patch) { uint32_t at = nvt_id(&c); NVT(&c.glob, 28, at, ty, nvt_ku(&c, td->n)); ty = at; }
        outv[i] = nvt_id(&c); NVT(&c.glob, 59, nvt_ptr(&c, 3, ty), outv[i], 3); NVT(&c.deco, 71, outv[i], 30, e->loc);
        if (e->patch) NVT(&c.deco, 71, outv[i], 15);
        iface[nif++] = outv[i];
    }
    if (pidloc != ~0u && t->pid_kind == 99) { snprintf(why, wl, "the post-tessellation patch_id is not a 32-bit integer"); goto refuse; }
    if (pidloc != ~0u) { pidout = nvt_id(&c); NVT(&c.glob, 59, nvt_ptr(&c, 3, c.t_vec[t->pid_kind][1]), pidout, 3); NVT(&c.deco, 71, pidout, 30, pidloc); NVT(&c.deco, 71, pidout, 15); iface[nif++] = pidout; }
    uint32_t main_ = nvt_id(&c);
    { const uint32_t pre[2] = { 1, main_ }; nvt_str(&c.ep, 15, pre, 2, "main", iface, nif); }
    NVT(&c.ep, 16, main_, 26, td->n);

    NVT(&c.fn, 54, c.t_void, main_, 0, c.t_fn); c.cur = nvt_id(&c); NVT(&c.fn, 248, c.cur);
    uint32_t pu64 = nvt_ptr(&c, 9, U64), pu32 = nvt_ptr(&c, 9, U), pf32 = nvt_ptr(&c, 9, F);
    #define NVT_PC(ptrty, ty, ...) ({ uint32_t p_ = nvt_id(&c), v_ = nvt_id(&c); NVT(&c.fn, 65, ptrty, p_, pc, __VA_ARGS__); NVT(&c.fn, 61, ty, v_, p_); v_; })
    uint32_t fac = NVT_PC(pu64, U64, nvt_ku(&c, 0)), cpx = NVT_PC(pu64, U64, nvt_ku(&c, 1)), pix = NVT_PC(pu64, U64, nvt_ku(&c, 2));
    uint32_t istr = NVT_PC(pu32, U, nvt_ku(&c, 3)), scl = NVT_PC(pf32, F, nvt_ku(&c, 4));
    uint32_t ids = ({ uint32_t p_ = nvt_id(&c), v_ = nvt_id(&c); NVT(&c.fn, 65, nvt_ptr(&c, 1, c.t_vec[NVT_U][2]), p_, inids, nvt_ku(&c, 0)); NVT(&c.fn, 61, c.t_vec[NVT_U][2], v_, p_); v_; });
    uint32_t pos = nvt_ex(&c, U, ids, 0), iid = nvt_ex(&c, U, ids, 1);
    uint32_t inv = ({ uint32_t v_ = nvt_id(&c); NVT(&c.fn, 61, I, v_, invid); nvt_op1(&c, 124, U, v_); });
    uint32_t z64 = nvt_k64(&c, 0);
    uint32_t pid = nvt_load_if(&c, nvt_op2(&c, 171, c.t_bool, pix, z64),
                               nvt_add64(&c, pix, nvt_op2(&c, 132, U64, nvt_widen(&c, pos), nvt_k64(&c, 4))), pos);
    uint32_t cpv = nvt_op2(&c, 128, U, nvt_op2(&c, 132, U, pid, nvt_ku(&c, td->n)), inv);
    if (td->cpidx) {
        uint32_t w4 = td->cpidx == 1 ? 2 : 4, byte = nvt_op2(&c, 132, U64, nvt_widen(&c, cpv), nvt_k64(&c, w4));
        uint32_t ea = nvt_add64(&c, cpx, byte), wa = nvt_op2(&c, 199, U64, ea, nvt_k64(&c, ~3ull));
        uint32_t word = nvt_load_if(&c, nvt_op2(&c, 171, c.t_bool, cpx, z64), wa, cpv);
        if (td->cpidx == 1) {
            uint32_t sh = nvt_op2(&c, 132, U, nvt_op2(&c, 199, U, nvt_op1(&c, 113, U, ea), nvt_ku(&c, 2)), nvt_ku(&c, 8));
            uint32_t hv = nvt_id(&c); NVT(&c.fn, 203, U, hv, word, sh, nvt_ku(&c, 16));
            cpv = nvt_sel(&c, U, nvt_op2(&c, 171, c.t_bool, cpx, z64), hv, cpv);
        } else cpv = word;
    }
    for (int i = 0; i < npl; i++) {
        const nvt_in *e = pl[i].in; uint32_t ty = c.t_vec[e->kind][e->comps], val;
        if (pl[i].sys) val = e->kind == NVT_U ? iid : nvt_op1(&c, 124, I, iid);
        else {
            uint32_t idx = pl[i].step == 4 ? cpv : pl[i].step == 3 ? pid : pl[i].step == 2 ? iid : nvt_ku(&c, 0);
            uint32_t vb = NVT_PC(pu64, U64, nvt_ku(&c, 5), nvt_ku(&c, pl[i].slot));
            uint32_t addr = nvt_add64(&c, nvt_add64(&c, vb, nvt_op2(&c, 132, U64, nvt_widen(&c, idx), nvt_k64(&c, pl[i].stride))), nvt_k64(&c, pl[i].off));
            val = nvt_pull(&c, pl[i].f, addr, e->kind, e->comps);
        }
        if (e->patch) NVT(&c.fn, 62, outv[i], val);
        else { uint32_t p_ = nvt_id(&c); NVT(&c.fn, 65, nvt_ptr(&c, 3, ty), p_, outv[i], inv); NVT(&c.fn, 62, p_, val); }
    }
    if (pidout) NVT(&c.fn, 62, pidout, t->pid_kind == NVT_U ? pid : nvt_op1(&c, 124, I, pid));
    uint32_t fsz = quad ? 12 : 8, fa = fac;
    if (td->stepfn & 2) fa = nvt_add64(&c, fa, nvt_op2(&c, 132, U64, nvt_widen(&c, iid), nvt_widen(&c, istr)));
    if (td->stepfn & 1) fa = nvt_add64(&c, fa, nvt_op2(&c, 132, U64, nvt_widen(&c, pos), nvt_k64(&c, fsz)));
    uint32_t h[3], e[6], ne = quad ? 4 : 3, ni = quad ? 2 : 1;
    for (uint32_t k = 0; k < (quad ? 3u : 2u); k++) h[k] = nvt_ext1(&c, c.t_vec[NVT_F][2], 62, nvt_load32(&c, k ? nvt_add64(&c, fa, nvt_k64(&c, 4 * k)) : fa));
    for (uint32_t k = 0; k < ne + ni; k++) e[k] = nvt_ex(&c, F, h[k / 2], k % 2);
    uint32_t ok = 0;
    for (uint32_t k = 0; k < ne; k++) { uint32_t g = nvt_op2(&c, 186, c.t_bool, e[k], nvt_kf(&c, 0.0f)); ok = k ? nvt_op2(&c, 167, c.t_bool, ok, g) : g; }
    uint32_t kmax = nvt_kf(&c, td->maxf), z = nvt_kf(&c, 0.0f);
    for (uint32_t k = 0; k < ne + ni; k++) {
        uint32_t x = e[k];
        if (td->scale_en) x = nvt_op2(&c, 133, F, x, scl);
        x = nvt_sel(&c, F, nvt_op2(&c, 186, c.t_bool, x, kmax), kmax, x);
        if (td->partition == 0) x = nvt_ext1(&c, F, 29, nvt_ext1(&c, F, 9, nvt_ext1(&c, F, 30, x)));
        if (k < ne) x = nvt_sel(&c, F, ok, x, z);
        uint32_t p_ = nvt_id(&c), dst = k < ne ? outer : inner, di = k < ne ? k : k - ne;
        NVT(&c.fn, 65, nvt_ptr(&c, 3, F), p_, dst, nvt_ki(&c, (int32_t)di)); NVT(&c.fn, 62, p_, x);
    }
    for (uint32_t k = ne; k < 4; k++) { uint32_t p_ = nvt_id(&c); NVT(&c.fn, 65, nvt_ptr(&c, 3, F), p_, outer, nvt_ki(&c, (int32_t)k)); NVT(&c.fn, 62, p_, z); }
    if (!quad) { uint32_t p_ = nvt_id(&c); NVT(&c.fn, 65, nvt_ptr(&c, 3, F), p_, inner, nvt_ki(&c, 1)); NVT(&c.fn, 62, p_, z); }
    NVT(&c.fn, 253); NVT(&c.fn, 56);
    #undef NVT_PC
    uint32_t *out = nvt_link(&c, n);
    if (!out) snprintf(why, wl, "out of memory building the tessellation control stage");
    return out;
refuse:
    free(c.caps.w); free(c.ep.w); free(c.deco.w); free(c.glob.w); free(c.fn.w);
    return NULL;
}
_Static_assert(sizeof(((nvk_pipeline *)0)->tess_vb) / sizeof(uint32_t) == NVMTL_TESS_NVB, "nvk_pipeline.tess_vb holds NVMTL_TESS_NVB slots");
_Static_assert(NVMTL_LOAD_ALL == (1u << (NVMTL_NCOL + 2)) - 1u, "NVMTL_LOAD_ALL must be every load bit: NVMTL_NCOL colour + depth + stencil");
_Static_assert(NVMTL_RPX_MAX == (1u << (NVMTL_NCOL + 2)) - 4u, "NVMTL_RPX_MAX must be every nonzero load key minus the 3 that rpv[] holds");

static void nvmtl_failed_pipeline(nvk_pipeline *pipeline)
{
    if (pipeline->pipe) pvkDestroyPipeline(g_dev, (VkPipeline)pipeline->pipe, NULL);
    if (pipeline->mod) pvkDestroyShaderModule(g_dev, (VkShaderModule)pipeline->mod, NULL);
    if (pipeline->layout) pvkDestroyPipelineLayout(g_dev, (VkPipelineLayout)pipeline->layout, NULL);
    if (pipeline->rp) pvkDestroyRenderPass(g_dev, (VkRenderPass)pipeline->rp, NULL);
    free(pipeline->ag_src);
    memset(pipeline, 0, sizeof *pipeline);
}
_Static_assert(sizeof(VkRenderPass) == sizeof(void *) && _Alignof(VkRenderPass) == _Alignof(void *), "VkRenderPass bridge ABI");
#define NVMTL_PIPELINE_STAGE(call, what) do { VkResult stage_result = (call); if (stage_result != VK_SUCCESS) { nvlog("vk: %s -> %d", what, stage_result); goto pipeline_failed; } } while (0)
static int nvmtl_vk_pipeline_create_impl(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, const void *blend_,
                                         const void *vin_, uint32_t samples, const nvk_tess_desc *td, const nvk_rt *rt, void *out_);
int nvmtl_vk_pipeline_create_vin_ms(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, const void *blend_,
                                    const void *vin_, uint32_t samples, void *out_)
{ return nvmtl_vk_pipeline_create_impl(vs, vn, fs, fn, vkfmt, depthfmt, blend_, vin_, samples, NULL, NULL, out_); }
int nvmtl_vk_pipeline_create_tess(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, const void *blend,
                                  const void *vin, uint32_t samples, const nvk_tess_desc *td, void *out)
{
    if (!td) { nvlog("tess pipeline REFUSED: no tessellation description"); return -1; }
    return nvmtl_vk_pipeline_create_impl(vs, vn, fs, fn, vkfmt, depthfmt, blend, vin, samples, td, NULL, out);
}
int nvmtl_vk_pipeline_create_rt(const void *vs, size_t vn, const void *fs, size_t fn, const nvk_rt *rt, const void *vin, uint32_t samples,
                                const nvk_tess_desc *td, void *out)
{
    if (!rt || rt->ncol > NVMTL_NCOL) { nvlog("pipeline REFUSED: %u colour attachments (this driver carries %u)", rt ? rt->ncol : 0u, NVMTL_NCOL); return -1; }
    if (nvmtl_vk_init()) return -1;
    uint32_t f0 = 0; while (f0 < rt->ncol && !rt->cfmt[f0]) f0++;
    for (uint32_t i = f0 + 1; !g_indep && i < rt->ncol; i++)
        if (rt->cfmt[i] && memcmp(&rt->blend[i], &rt->blend[f0], sizeof rt->blend[i])) {
            nvlog("pipeline REFUSED: colour attachments %u and %u blend differently and this device has no independentBlend", f0, i); return -1; }
    return nvmtl_vk_pipeline_create_impl(vs, vn, fs, fn, rt->ncol ? rt->cfmt[0] : 0u, rt->dfmt, NULL, vin, samples, td, rt, out);
}
static int nvmtl_vk_pipeline_create_impl_lifetime_body(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, const void *blend_,
                                         const void *vin_, uint32_t samples, const nvk_tess_desc *td, const nvk_rt *rt, void *out_);
static int nvmtl_vk_pipeline_create_impl(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, const void *blend_,
                                         const void *vin_, uint32_t samples, const nvk_tess_desc *td, const nvk_rt *rt, void *out_)
{
    nvk_pipeline pending = {0};
    int result = nvmtl_vk_pipeline_create_impl_lifetime_body(vs, vn, fs, fn, vkfmt, depthfmt, blend_, vin_, samples, td, rt, &pending);
    if (result) { nvmtl_failed_pipeline(&pending); memset(out_, 0, sizeof pending); }
    else *(nvk_pipeline *)out_ = pending;
    return result;
}
static int nvmtl_vk_pipeline_create_impl_lifetime_body(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, const void *blend_,
                                         const void *vin_, uint32_t samples, const nvk_tess_desc *td, const nvk_rt *rt, void *out_)
{
    const nvk_blend_state *blend = (const nvk_blend_state *)blend_; const nvk_vertex_input *vin = (const nvk_vertex_input *)vin_; nvk_pipeline *out = (nvk_pipeline *)out_;
    nvk_rt lrt;
    if (!rt) {
        memset(&lrt, 0, sizeof lrt); lrt.ncol = vkfmt ? 1u : 0u; lrt.cfmt[0] = vkfmt; lrt.dfmt = depthfmt;
        lrt.blend[0] = blend ? *blend : (nvk_blend_state){ VK_FALSE, VK_BLEND_FACTOR_ONE, VK_BLEND_FACTOR_ZERO, VK_BLEND_OP_ADD,
                                                           VK_BLEND_FACTOR_ONE, VK_BLEND_FACTOR_ZERO, VK_BLEND_OP_ADD, 0xf };
        rt = &lrt;
    }
    if (samples != 1 && samples != 2 && samples != 4 && samples != 8) { nvlog("pipeline: rasterSampleCount %u is not carried (1, 2, 4, 8)", samples); return -1; }
    NSData *vdata = nvmtl_address_shader(vs, vn), *fdata = nvmtl_address_shader(fs, fn);
    if (!vdata || !fdata) return -1;
    vs = vdata.bytes; vn = vdata.length; fs = fdata.bytes; fn = fdata.length;
    if (nvmtl_vk_init()) return -1;
    memset(out, 0, sizeof *out);
    NSData *tvsd = nil, *ttcd = nil;
    if (td) {
        if (!g_tess) { nvlog("tess pipeline REFUSED: this device reports no tessellationShader"); return -1; }
        nvt_tese t0;
        if (nvt_scan_tese((const uint32_t *)vs, vn / 4, &t0) || t0.model != 2) { nvlog("tess pipeline REFUSED: the vertex module is not a TessellationEvaluation module after address lowering"); return -1; }
        uint32_t spacing = td->partition == 2 ? 3 : td->partition == 3 ? 2 : 1;
        const char *flip = getenv("NVMTL_TESS_ORDER_FLIP"); int fl = flip && *flip == '1';
        uint32_t order = ((td->winding == 0) != fl) ? 5 : 4;
        size_t ten = 0, tcn = 0, gvn = 0; uint32_t pidloc = ~0u, nvb = 0; const char *why = NULL; char why2[256] = "";
        uint32_t *te = nvmtl_tess_patch_tese((const uint32_t *)vs, vn / 4, spacing, order, &ten, &pidloc, &why);
        if (!te) { nvlog("tess pipeline REFUSED: %s", why ? why : "?"); return -1; }
        uint32_t *tc = nvmtl_tess_make_tcs(&t0, td, vin, pidloc, out->tess_vb, &nvb, &tcn, why2, sizeof why2);
        uint32_t *gv = tc ? nvmtl_tess_make_vs(&gvn) : NULL;
        if (!tc || !gv) { nvlog("tess pipeline REFUSED: %s", tc ? "out of memory building the tessellation vertex stage" : why2); free(te); free(tc); return -1; }
        vdata = [NSData dataWithBytesNoCopy:te length:ten * 4 freeWhenDone:YES];
        ttcd = [NSData dataWithBytesNoCopy:tc length:tcn * 4 freeWhenDone:YES];
        tvsd = [NSData dataWithBytesNoCopy:gv length:gvn * 4 freeWhenDone:YES];
        vs = vdata.bytes; vn = vdata.length;
        out->tess = 1; out->tess_n = td->n; out->tess_cpidx = td->cpidx; out->tess_nvb = nvb;
        nvlog("tess pipeline: %s patch, %u control points, partition %u, winding %u%s, factor step %u, scale %u, cp index %u, max %.0f, "
              "%u vertex buffer(s) pulled, patch_id at Location %d, instance_id Location %d",
              td->patch_type == 2 ? "quad" : "triangle", td->n, td->partition, td->winding, fl ? " (vertex order STRAIGHT by NVMTL_TESS_ORDER_FLIP - the negative control)" : "",
              td->stepfn, td->scale_en, td->cpidx, (double)td->maxf, nvb, (int)pidloc, (int)td->inst_loc);
    }
    VkVertexInputBindingDescription vib[NVMTL_NVIN]; VkVertexInputAttributeDescription via[NVMTL_NVIN]; uint32_t nvib = 0, nvia = 0;
    out->vin_dyn = 0;
    if (!td && nvmtl_vertex_input_build(vin, vs, vn, vib, &nvib, via, &nvia, &out->vin_mask, &out->vin_dyn, out->vin_stride)) return -1;
    if (out->vin_dyn && !pvkCmdBindVertexBuffers2) { nvlog("apps-dyn pipeline: layout(s) %#x are MTLBufferLayoutStrideDynamic and "
        "vkCmdBindVertexBuffers2 is not loaded — REFUSED", out->vin_dyn); return -1; }
    int hasDepth = rt->dfmt != 0;
    out->has_depth = hasDepth; out->dfmt = rt->dfmt; out->cfmt = rt->ncol ? rt->cfmt[0] : 0u; out->samples = samples;
    out->ncol = rt->ncol; memcpy(out->cfmts, rt->cfmt, sizeof out->cfmts);
    out->has_stencil = rt->dfmt >= VK_FORMAT_S8_UINT && rt->dfmt <= VK_FORMAT_D32_SFLOAT_S8_UINT;
    for (uint32_t i = 0; samples > 1 && i < rt->ncol; i++) if (rt->cfmt[i]) out->rmask |= 1u << i;
    out->stex_hi[0] = nvmtl_spirv_stex_hi(vs, vn); out->stex_hi[1] = nvmtl_spirv_stex_hi(fs, fn);
    nvmtl_spirv_texel_mask(vs, vn, out->tbm[0]); nvmtl_spirv_texel_mask(fs, fn, out->tbm[1]);
    out->tbany = 0; for (int k = 0; k < 4; k++) out->tbany |= (out->tbm[0][k] | out->tbm[1][k]) != 0;
    (void)vkfmt;
    VkRenderPass pass = VK_NULL_HANDLE;
    VKCK(nvmtl_make_render_pass(out, 0, &pass), "vkCreateRenderPass");
    out->rp = pass;

    if (!nvmtl_spirv_ok(vs, vn, "vertex module") || !nvmtl_spirv_ok(fs, fn, "fragment module")) return -1;
    if (!td) out->gmask_ok = (uint32_t)(nvmtl_spirv_bmask(vs, vn, out->gmask[0]) && nvmtl_spirv_bmask(fs, fn, out->gmask[1]));
    VkShaderModuleCreateInfo vsm = { VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, NULL, 0, vn, (const uint32_t *)vs };
    uint32_t *fcopy = (uint32_t *)malloc(fn);
    if (!fcopy) { nvlog("vk: out of memory copying the fragment module"); return -1; }
    memcpy(fcopy, fs, fn);
    if (ensure_descriptors()) { free(fcopy); return -1; }
    if (!g_heapSet && (nvmtl_spirv_uses_set(vs, vn / 4, 2) || nvmtl_spirv_uses_set(fs, fn / 4, 2))) {
        free(fcopy); nvlog("vk: render pipeline REFUSED - a shader indexes the bindless heap (set 2) and this device has none"); return -1; }
    nvmtl_spirv_retarget_set(fcopy, fn / 4, 1);
    VkShaderModuleCreateInfo fsm = { VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, NULL, 0, fn, fcopy };
    VkShaderModule vmod = VK_NULL_HANDLE, fmod = VK_NULL_HANDLE, tvm = VK_NULL_HANDLE, tcm = VK_NULL_HANDLE;
    NVMTL_PIPELINE_STAGE(pvkCreateShaderModule(g_dev, &vsm, NULL, &vmod), "vkCreateShaderModule(vertex)");
    NVMTL_PIPELINE_STAGE(pvkCreateShaderModule(g_dev, &fsm, NULL, &fmod), "vkCreateShaderModule(fragment)");
    VkPipelineShaderStageCreateInfo stages[4] = {
        { VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0, td ? VK_SHADER_STAGE_TESSELLATION_EVALUATION_BIT : VK_SHADER_STAGE_VERTEX_BIT, vmod, "main", NULL },
        { VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0, VK_SHADER_STAGE_FRAGMENT_BIT, fmod, "main", NULL } };
    uint32_t nstages = 2;
    if (td) {
        VkShaderModuleCreateInfo tvi = { VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, NULL, 0, tvsd.length, (const uint32_t *)tvsd.bytes };
        VkShaderModuleCreateInfo tci = { VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, NULL, 0, ttcd.length, (const uint32_t *)ttcd.bytes };
        NVMTL_PIPELINE_STAGE(pvkCreateShaderModule(g_dev, &tvi, NULL, &tvm), "vkCreateShaderModule(tessellation vertex)");
        NVMTL_PIPELINE_STAGE(pvkCreateShaderModule(g_dev, &tci, NULL, &tcm), "vkCreateShaderModule(tessellation control)");
        stages[nstages++] = (VkPipelineShaderStageCreateInfo){ VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0, VK_SHADER_STAGE_VERTEX_BIT, tvm, "main", NULL };
        stages[nstages++] = (VkPipelineShaderStageCreateInfo){ VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0, VK_SHADER_STAGE_TESSELLATION_CONTROL_BIT, tcm, "main", NULL };
    }

    if (ensure_descriptors()) goto pipeline_failed;
    VkDescriptorSetLayout sl2[3] = { g_setLayout, g_setLayout, g_heapLayout };
    VkPushConstantRange pc[2]; uint32_t npc = 0;
    if (td) pc[npc++] = (VkPushConstantRange){ VK_SHADER_STAGE_TESSELLATION_CONTROL_BIT, 0, NVMTL_TESS_PC_BYTES };
    if (g_sample_locations) pc[npc++] = (VkPushConstantRange){ VK_SHADER_STAGE_FRAGMENT_BIT, NVMTL_SAMPLE_POSITION_OFFSET, NVMTL_SAMPLE_POSITION_BYTES };
    VkPipelineLayoutCreateInfo plci = { VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, NULL, 0, g_heapSet ? 3 : 2, sl2, npc, npc ? pc : NULL };
    NVMTL_PIPELINE_STAGE(nvmtl_opaque_CreatePipelineLayout(g_dev, &plci, NULL, &out->layout), "vkCreatePipelineLayout");

    VkPipelineVertexInputStateCreateInfo vi = { VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO, NULL, 0, nvib, vib, nvia, via };
    VkPipelineInputAssemblyStateCreateInfo ia = { VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, NULL, 0,
        td ? VK_PRIMITIVE_TOPOLOGY_PATCH_LIST : VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST, VK_FALSE };
    VkPipelineTessellationStateCreateInfo tss = { VK_STRUCTURE_TYPE_PIPELINE_TESSELLATION_STATE_CREATE_INFO, NULL, 0, 1 };
    VkPipelineViewportStateCreateInfo vps = { VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, NULL, 0, 1, NULL, 1, NULL };
    VkDynamicState dyn[16] = { VK_DYNAMIC_STATE_VIEWPORT, VK_DYNAMIC_STATE_SCISSOR, VK_DYNAMIC_STATE_CULL_MODE,
        VK_DYNAMIC_STATE_BLEND_CONSTANTS, VK_DYNAMIC_STATE_STENCIL_REFERENCE, VK_DYNAMIC_STATE_DEPTH_BIAS,
        VK_DYNAMIC_STATE_PRIMITIVE_TOPOLOGY, VK_DYNAMIC_STATE_PRIMITIVE_RESTART_ENABLE, VK_DYNAMIC_STATE_FRONT_FACE,
        VK_DYNAMIC_STATE_DEPTH_TEST_ENABLE, VK_DYNAMIC_STATE_DEPTH_WRITE_ENABLE, VK_DYNAMIC_STATE_DEPTH_COMPARE_OP,
        VK_DYNAMIC_STATE_STENCIL_TEST_ENABLE, VK_DYNAMIC_STATE_STENCIL_OP, VK_DYNAMIC_STATE_STENCIL_COMPARE_MASK, VK_DYNAMIC_STATE_STENCIL_WRITE_MASK };
    VkPipelineDynamicStateCreateInfo dsci = { VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO, NULL, 0,
        hasDepth ? (out->has_stencil ? 16u : 12u) : 9u, dyn };
    VkDynamicState dyn3s[18];
    if (g_dyn3) {
        uint32_t n3 = 0;
        for (uint32_t i = 0; i < (hasDepth ? (out->has_stencil ? 16u : 12u) : 9u); i++) dyn3s[n3++] = dyn[i];
        dyn3s[n3++] = VK_DYNAMIC_STATE_POLYGON_MODE_EXT;
        if (g_clip) dyn3s[n3++] = VK_DYNAMIC_STATE_DEPTH_CLIP_ENABLE_EXT;
        dsci.pDynamicStates = dyn3s; dsci.dynamicStateCount = n3;
    }
    VkDynamicState dynv[20];
    if (out->vin_dyn) {
        uint32_t nv = 0;
        for (uint32_t i = 0; i < dsci.dynamicStateCount; i++) dynv[nv++] = dsci.pDynamicStates[i];
        dynv[nv++] = VK_DYNAMIC_STATE_VERTEX_INPUT_BINDING_STRIDE;
        dsci.pDynamicStates = dynv; dsci.dynamicStateCount = nv;
    }
    VkDynamicState dynl[21];
    if (g_wide) {
        uint32_t nl = 0;
        for (uint32_t i = 0; i < dsci.dynamicStateCount; i++) dynl[nl++] = dsci.pDynamicStates[i];
        dynl[nl++] = VK_DYNAMIC_STATE_LINE_WIDTH;
        dsci.pDynamicStates = dynl; dsci.dynamicStateCount = nl;
    }
    VkDynamicState dynsample[22];
    if (g_sample_locations) {
        for (uint32_t i = 0; i < dsci.dynamicStateCount; ++i) dynsample[i] = dsci.pDynamicStates[i];
        dynsample[dsci.dynamicStateCount++] = VK_DYNAMIC_STATE_SAMPLE_LOCATIONS_EXT;
        dsci.pDynamicStates = dynsample;
    }
    VkPipelineRasterizationStateCreateInfo rs = { VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO, NULL, 0,
        VK_FALSE, rt->discard ? VK_TRUE : VK_FALSE, VK_POLYGON_MODE_FILL, VK_CULL_MODE_NONE, VK_FRONT_FACE_COUNTER_CLOCKWISE, VK_FALSE, 0, 0, 0, 1.0f };
    if (rt->a2one && !g_a2one) { nvlog("pipeline REFUSED: alphaToOneEnabled and this device has no alphaToOne feature"); goto pipeline_failed; }
    VkPipelineMultisampleStateCreateInfo ms = { VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO, NULL, 0,
        (VkSampleCountFlagBits)(samples > 1 ? samples : 1), VK_FALSE, 0, NULL,
        rt->a2c ? VK_TRUE : VK_FALSE, rt->a2one ? VK_TRUE : VK_FALSE };
    nvmtl_sample_pattern default_sample;
    VkSampleLocationEXT default_vk[8];
    VkPipelineSampleLocationsStateCreateInfoEXT sample_state = { VK_STRUCTURE_TYPE_PIPELINE_SAMPLE_LOCATIONS_STATE_CREATE_INFO_EXT };
    if (g_sample_locations) {
        if (nvmtl_sample_default(samples > 1 ? samples : 1, &default_sample)) goto pipeline_failed;
        sample_state.sampleLocationsEnable = VK_TRUE;
        sample_state.sampleLocationsInfo = nvmtl_sample_info(&default_sample, default_vk);
        ms.pNext = &sample_state;
    }
    VkPipelineColorBlendAttachmentState cba[NVMTL_NCOL];
    for (uint32_t i = 0; i < rt->ncol; i++) {
        const nvk_blend_state *b = &rt->blend[i];
        cba[i] = rt->cfmt[i]
            ? (VkPipelineColorBlendAttachmentState){ b->enabled, b->src_rgb, b->dst_rgb, b->op_rgb, b->src_alpha, b->dst_alpha, b->op_alpha, b->write_mask }
            : (VkPipelineColorBlendAttachmentState){ VK_FALSE, VK_BLEND_FACTOR_ONE, VK_BLEND_FACTOR_ZERO, VK_BLEND_OP_ADD,
                                                     VK_BLEND_FACTOR_ONE, VK_BLEND_FACTOR_ZERO, VK_BLEND_OP_ADD, 0 };
    }
    VkPipelineColorBlendStateCreateInfo cb = { VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO, NULL, 0,
        VK_FALSE, VK_LOGIC_OP_COPY, rt->ncol, rt->ncol ? cba : NULL, { 0, 0, 0, 0 } };
    VkPipelineDepthStencilStateCreateInfo ds = { VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO, NULL, 0,
        VK_TRUE, VK_TRUE, VK_COMPARE_OP_LESS, VK_FALSE, VK_FALSE, { 0 }, { 0 }, 0.0f, 1.0f };
    VkGraphicsPipelineCreateInfo gp = { VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO, NULL, 0, nstages, stages,
        &vi, &ia, td ? &tss : NULL, &vps, &rs, &ms, hasDepth ? &ds : NULL, &cb, &dsci, out->layout, out->rp, 0, VK_NULL_HANDLE, -1 };
    NVMTL_PIPELINE_STAGE(nvmtl_opaque_CreateGraphicsPipelines(g_dev, VK_NULL_HANDLE, 1, &gp, NULL, &out->pipe), "vkCreateGraphicsPipelines");
    if (tcm) pvkDestroyShaderModule(g_dev, tcm, NULL);
    if (tvm) pvkDestroyShaderModule(g_dev, tvm, NULL);
    pvkDestroyShaderModule(g_dev, fmod, NULL);
    pvkDestroyShaderModule(g_dev, vmod, NULL);
    free(fcopy);
    return 0;
pipeline_failed:
    if (tcm) pvkDestroyShaderModule(g_dev, tcm, NULL);
    if (tvm) pvkDestroyShaderModule(g_dev, tvm, NULL);
    if (fmod) pvkDestroyShaderModule(g_dev, fmod, NULL);
    if (vmod) pvkDestroyShaderModule(g_dev, vmod, NULL);
    free(fcopy);
    return -1;
}

int nvmtl_vk_pipeline_create_fmt(const void *vs, size_t vn, const void *fs, size_t fn, int bgra, nvk_pipeline *out)
{ return nvmtl_vk_pipeline_create_depth(vs, vn, fs, fn, bgra, 0, out); }

int nvmtl_vk_pipeline_create(const void *vs, size_t vn, const void *fs, size_t fn, nvk_pipeline *out)
{ return nvmtl_vk_pipeline_create_fmt(vs, vn, fs, fn, 0, out); }

int nvmtl_vk_sampler_create(int linear, int repeat, nvk_sampler *out)
{
    if (!out) return -1;
    memset(out, 0, sizeof *out);
    if (nvmtl_vk_init()) return -1;
    VkSamplerAddressMode am = repeat ? VK_SAMPLER_ADDRESS_MODE_REPEAT : VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE;
    VkSamplerCreateInfo sci = { VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO };
    sci.magFilter = sci.minFilter = linear ? VK_FILTER_LINEAR : VK_FILTER_NEAREST;
    sci.mipmapMode = linear ? VK_SAMPLER_MIPMAP_MODE_LINEAR : VK_SAMPLER_MIPMAP_MODE_NEAREST;
    sci.addressModeU = sci.addressModeV = sci.addressModeW = am;
    sci.maxLod = VK_LOD_CLAMP_NONE;
    VkSampler handle = VK_NULL_HANDLE;
    VKCK(nvmtl_sampler_acquire(&sci, &handle), "sampler acquire");
    out->s = (void *)handle;
    out->linear = linear; out->repeat = repeat; out->lod_max = VK_LOD_CLAMP_NONE;
    return 0;
}

int nvmtl_vk_sampler_create_desc(const nvmtl_sampler_desc *desc, nvk_sampler *out)
{
    if (!out) return -1;
    nvmtl_sampler_desc d = {0}; if (desc) d = *desc;
    memset(out, 0, sizeof *out);
    if (!desc || nvmtl_vk_init()) return -1;
    const nvmtl_sampler_caps caps = { g_sampler_anisotropy, g_sampler_mirror_clamp, g_lim.maxSamplerAnisotropy };
    VkSamplerCreateInfo sci;
    if (nvmtl_sampler_info(&d, &caps, &sci)) { nvlog("sampler descriptor unsupported or invalid - refused, not substituted"); return -1; }
    VkSampler h = VK_NULL_HANDLE;
    VkResult r = nvmtl_sampler_acquire(&sci, &h);
    if (r != VK_SUCCESS || h == VK_NULL_HANDLE) { nvlog("vkCreateSampler (full state) -> %d", r); return -1; }
    out->s = (void *)h; out->desc = d; out->full_state = 1; out->lod_min = d.lod_min; out->lod_max = d.lod_max;
    out->linear = d.min_filter || d.mag_filter; out->repeat = d.address_s == 2;
    return 0;
}

void nvmtl_vk_sampler_destroy(nvk_sampler *s)
{
    if (!s || !s->s) return;
    VkSampler handle = (VkSampler)s->s;
    s->s = NULL;
    nvmtl_sampler_release(handle);
}

static int nvmtl_vk_compute_pipeline_create_g(const void *cs, size_t n, uint32_t flags, nvk_pipeline *out);
int nvmtl_vk_compute_pipeline_create(const void *cs, size_t n, nvk_pipeline *out) { return nvmtl_vk_compute_pipeline_create_g(cs, n, 0, out); }
static int nvmtl_vk_compute_pipeline_create_g_lifetime_body(const void *cs, size_t n, uint32_t flags, nvk_pipeline *out);
static int nvmtl_vk_compute_pipeline_create_g(const void *cs, size_t n, uint32_t flags, nvk_pipeline *out)
{
    nvk_pipeline pending = {0};
    int result = nvmtl_vk_compute_pipeline_create_g_lifetime_body(cs, n, flags, &pending);
    if (result) { nvmtl_failed_pipeline(&pending); memset(out, 0, sizeof pending); }
    else *(nvk_pipeline *)out = pending;
    return result;
}
static int nvmtl_vk_compute_pipeline_create_g_lifetime_body(const void *cs, size_t n, uint32_t flags, nvk_pipeline *out)
{
    const void *const ag_src = cs; const size_t ag_srcn = n; uint32_t ag[4];
    NSData *data = nvmtl_address_shader_g(cs, n, flags, ag); if (!data) return -1;
    if (!(flags & 1) && ag[2]) {
        const unsigned long long k = atomic_fetch_add(&g_ag_untraceable, 1) + 1;
        if (!(k & (k - 1))) nvlog("align guard: %llu compute module(s) built SAFE - a promised access could not be traced to a binding", k);
        data = nvmtl_address_shader_g(cs, n, 1, NULL); if (!data) return -1;
        ag[0] = 0;
    }
    cs = data.bytes; n = data.length;
    if (nvmtl_vk_init()) return -1;
    if (ensure_descriptors()) return -1;
    if (!g_heapSet && nvmtl_spirv_uses_set(cs, n / 4, 2)) {
        nvlog("vk: compute pipeline REFUSED - the kernel indexes the bindless heap (set 2) and this device has none"); return -1; }
    memset(out, 0, sizeof *out);
    if (!(flags & 1) && ag[0]) {
        if (!(out->ag_src = malloc(ag_srcn))) { nvlog("align guard: out of memory keeping a module's source - pipeline REFUSED"); return -1; }
        memcpy(out->ag_src, ag_src, ag_srcn); out->ag_srcn = ag_srcn; out->ag_mask = ag[0]; out->ag_req = ag[1] >= 8 ? ag[1] : 8;
        atomic_fetch_add(&g_ag_guarded, 1);
    }
    if (!nvmtl_spirv_ok(cs, n, "compute module")) return -1;
    out->stex_hi[0] = nvmtl_spirv_stex_hi(cs, n);
    nvmtl_spirv_texel_mask(cs, n, out->tbm[0]); memset(out->tbm[1], 0, sizeof out->tbm[1]);
    out->tbany = (out->tbm[0][0] | out->tbm[0][1] | out->tbm[0][2] | out->tbm[0][3]) != 0;
    out->lin = nvmtl_spirv_has_specid(cs, n / 4, 3);
    out->bmask_ok = (uint32_t)nvmtl_spirv_bmask(cs, n, out->bmask);
    VkShaderModuleCreateInfo smi = { VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, NULL, 0, n, (const uint32_t *)cs };
    VkShaderModule mod = VK_NULL_HANDLE;
    VKCK(pvkCreateShaderModule(g_dev, &smi, NULL, &mod), "vkCreateShaderModule(compute)");
    out->mod = mod;
    VkDescriptorSetLayout sl2c[3] = { g_setLayout, g_setLayout, g_heapLayout };
    VkPushConstantRange pcr = { VK_SHADER_STAGE_COMPUTE_BIT, 0, NVMTL_DISPATCH_PC_BYTES };
    VkPipelineLayoutCreateInfo plci = { VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, NULL, 0, g_heapSet ? 3 : 2, sl2c, 1, &pcr };
    VKCK(nvmtl_opaque_CreatePipelineLayout(g_dev, &plci, NULL, &out->layout), "vkCreatePipelineLayout(compute)");
    VkComputePipelineCreateInfo cpi = { VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO, NULL, 0,
        { VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0, VK_SHADER_STAGE_COMPUTE_BIT, mod, "main", NULL },
        out->layout, VK_NULL_HANDLE, -1 };
    VKCK(nvmtl_opaque_CreateComputePipelines(g_dev, VK_NULL_HANDLE, 1, &cpi, NULL, &out->pipe), "vkCreateComputePipelines");
    out->mod = mod;
    return 0;
}

int nvmtl_vk_tg_reject(const uint32_t l[3], const uint32_t *t, const char *kernel, uint32_t lin)
{
    static _Atomic unsigned zero_n, over_n, lin_n;
    if (!l[0] || !l[1] || !l[2] || (t && (!t[0] || !t[1] || !t[2]))) {
        if (!zero_n++) nvlog("dispatch: zero dimension (%s grid %ux%ux%u tg %ux%ux%u) is a silent no-op, as on Apple - first of its kind in this process",
            kernel ? kernel : "-", t ? t[0] : 0, t ? t[1] : 0, t ? t[2] : 0, l[0], l[1], l[2]);
        return 1;
    }
    if (g_state != 1) return 0;
    const uint32_t *w = g_lim.maxComputeWorkGroupSize;
    if (lin && (l[0] > w[0] || l[1] > w[1] || l[2] > w[2]) && (uint64_t)l[0] * l[1] * l[2] <= g_lim.maxComputeWorkGroupInvocations) {
        if (!lin_n++) nvlog("dispatch: %s threadgroup %ux%ux%u runs LINEARIZED as %ux1x1 (SpecId 3) - over the per-dimension limit %ux%ux%u, inside %u invocations",
            kernel ? kernel : "-", l[0], l[1], l[2], l[0] * l[1] * l[2], w[0], w[1], w[2], g_lim.maxComputeWorkGroupInvocations);
        return 0;
    }
    if (l[0] > w[0] || l[1] > w[1] || l[2] > w[2] || (uint64_t)l[0] * l[1] * l[2] > g_lim.maxComputeWorkGroupInvocations) {
        unsigned n = ++over_n;
        if (n <= 16 || !(n & 1023))
            nvlog("dispatch REFUSED: %s threadgroup %ux%ux%u exceeds the hardware limit %ux%ux%u (%u invocations) - launched, QMD CTA_THREAD_DIMENSION2 (8 bits) wraps and SKEDCHECK16 kills the channel (Xid 13); skipped [%u so far]",
                  kernel ? kernel : "-", l[0], l[1], l[2], w[0], w[1], w[2], g_lim.maxComputeWorkGroupInvocations, n);
        return 1;
    }
    return 0;
}
static pthread_mutex_t g_var_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t g_ag_lock = PTHREAD_MUTEX_INITIALIZER;
static nvk_pipeline *nvmtl_align_guard_pick(nvk_cmdbuf *c, nvk_pipeline *p)
{
    if (!p->ag_mask) return p;
    const nvmtl_bindtable *t = (const nvmtl_bindtable *)c->table;
    const uint64_t m = (uint64_t)p->ag_req - 1;
    int bad = -1; uint64_t at = 0;
    for (uint32_t b = 0; b < 32 && b < NVMTL_NBUF; b++) {
        if (!(p->ag_mask & (1u << b))) continue;
        const nvmtl_slot *s = t ? &t->s[0][b] : NULL;
        if (s && s->kind == NVMTL_SLOT_BUF && ((s->address + s->off) & m)) { bad = (int)b; at = s->address + s->off; break; }
    }
    if (bad < 0) return p;
    pthread_mutex_lock(&g_ag_lock);
    nvk_pipeline *sp = (nvk_pipeline *)p->ag_safe;
    if (!sp && p->ag_src) {
        sp = calloc(1, sizeof *sp);
        if (sp && nvmtl_vk_compute_pipeline_create_g(p->ag_src, p->ag_srcn, 1, sp)) { free(sp); sp = NULL; }
        p->ag_safe = sp;
        nvlog("align guard: buffer %d bound at GPU address %#llx is not %u-aligned - %s", bad, (unsigned long long)at, p->ag_req,
              sp ? "this kernel now runs its SAFE module on such bindings (Apple runs a broken alignment promise exactly)" : "SAFE module FAILED - such dispatches are SKIPPED");
    }
    pthread_mutex_unlock(&g_ag_lock);
    const unsigned long long k = atomic_fetch_add(&g_ag_switched, 1) + 1;
    if (!(k & (k - 1))) nvlog("align guard: %llu dispatch(es) ran a SAFE module (%llu modules guarded)", k, (unsigned long long)atomic_load(&g_ag_guarded));
    return sp;
}
static VkPipeline nvmtl_compute_variant(nvk_pipeline *p, const uint32_t l[3])
{
    if (nvmtl_vk_tg_reject(l, NULL, "(variant)", p->lin)) return VK_NULL_HANDLE;
    if (!p->mod) return (VkPipeline)p->pipe;
    VkPipeline found = VK_NULL_HANDLE;
    pthread_mutex_lock(&g_var_lock);
    for (uint32_t i = 0; i < p->nvar; i++) if (!memcmp(p->var[i].l, l, sizeof p->var[i].l)) { found = (VkPipeline)p->var[i].pipe; break; }
    if (!found) {
        const uint32_t *wl = g_lim.maxComputeWorkGroupSize;
        uint32_t sd[4] = { l[0], l[1], l[2], (p->lin && (l[0] > wl[0] || l[1] > wl[1] || l[2] > wl[2])) ? 1u : 0u };
        VkSpecializationMapEntry me[4] = { { 0, 0, 4 }, { 1, 4, 4 }, { 2, 8, 4 }, { 3, 12, 4 } };
        VkSpecializationInfo si = { 4, me, 16, sd };
        VkComputePipelineCreateInfo cpi = { VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO, NULL, 0,
            { VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0, VK_SHADER_STAGE_COMPUTE_BIT, (VkShaderModule)p->mod, "main", &si },
            (VkPipelineLayout)p->layout, VK_NULL_HANDLE, -1 };
        VkResult r = pvkCreateComputePipelines(g_dev, VK_NULL_HANDLE, 1, &cpi, NULL, &found);
        if (r != VK_SUCCESS) {
            if (found) pvkDestroyPipeline(g_dev, found, NULL);
            nvlog("vk: compute variant %ux%ux%u -> %d — this dispatch is SKIPPED", l[0], l[1], l[2], r); found = VK_NULL_HANDLE;
        }
        else {
            if (p->nvar == p->capvar && p->capvar < NVMTL_NVAR_MAX) {
                uint32_t cap = p->capvar ? p->capvar * 2 : NVMTL_NVAR;
                struct nvmtl_cvar *g = realloc(p->var, cap * sizeof *g);
                if (g) { p->var = g; p->capvar = cap;
                    if (cap > NVMTL_NVAR) nvlog("vk: cvar1 one kernel now has %u threadgroup-size variants (table grown to %u)", p->nvar, cap); }
            }
            if (p->nvar < p->capvar) { memcpy(p->var[p->nvar].l, l, sizeof p->var[p->nvar].l); p->var[p->nvar].pipe = found; p->nvar++; }
            else { static int said; if (!said++) nvlog("vk: one kernel was dispatched with more than %u threadgroup sizes — variants now leak", p->capvar); }
        }
    }
    pthread_mutex_unlock(&g_var_lock);
    return found;
}

int nvmtl_vk_cmd_dispatch_sass(nvk_cmdbuf *c,
                               const void *sass, uint32_t sass_len,
                               uint32_t regs, uint32_t smem, uint32_t barriers, uint32_t nparams,
                               const uint32_t threads[3], const uint32_t tg[3])
{
    static int (*hook)(void *, const void *, uint32_t, uint32_t, uint32_t, uint32_t,
                       const uint64_t *, uint32_t, const uint32_t *, const uint32_t *);
    static int looked;
    uint64_t params[16];
    uint32_t grid[3];

    if (!c || !c->cb || !sass || !sass_len) return -1;
    if (nparams > 16) { nvlog("sass dispatch: %u parameters is more than 16 — REFUSED", nparams); return -2; }

    if (!looked) {

        const char *path = getenv("NVMTL_NVK_DYLIB");
        void *h = dlopen(path && *path ? path : "/Library/GPUBundles/nvmtl/libvulkan_nouveau.dylib",
                         RTLD_NOW | RTLD_LOCAL);
        looked = 1;
        if (!h) nvlog("sass dispatch: cannot open NVK: %s", dlerror());
        else {
            hook = (int (*)(void *, const void *, uint32_t, uint32_t, uint32_t, uint32_t,
                            const uint64_t *, uint32_t, const uint32_t *, const uint32_t *))
                       dlsym(h, "nvmtl_nvk_dispatch_sass_v1");
            nvlog("sass dispatch: NVK hook %s", hook ? "found" : "MISSING — this NVK predates it");
        }
    }
    if (!hook) return -3;

    for (int d = 0; d < 3; d++) {
        if (!tg[d] || !threads[d]) return -4;
        if (threads[d] % tg[d]) {
            nvlog("sass dispatch: %u threads is not a whole number of %u-wide groups on axis %d — "
                  "REFUSED, the SPIR-V path owns the partial tail", threads[d], tg[d], d);
            return -5;
        }
        grid[d] = threads[d] / tg[d];
    }

    if (!c->table) { nvlog("sass dispatch: no bind table"); return -6; }
    nvmtl_bindtable *t = (nvmtl_bindtable *)c->table;
    for (uint32_t i = 0; i < nparams; i++) {
        nvmtl_slot *s = &t->s[0][i];
        if (s->kind != NVMTL_SLOT_BUF || !s->address) {
            nvlog("sass dispatch: parameter %u wants a buffer at [[buffer(%u)]] and slot kind is %u "
                  "— REFUSED", i, i, s->kind);
            return -7;
        }
        params[i] = s->address + (uint64_t)s->off;
    }

    int rc = hook(c->cb, sass, sass_len, regs, smem, barriers, params, nparams, tg, grid);
    if (rc) { nvlog("sass dispatch: NVK refused it, rc=%d", rc); return -8; }
    if (nvmtl_trace_on())
        nvtrace("SASS DISPATCH %u B, regs %u, %u param(s) | tg %ux%ux%u grid %ux%ux%u | p0 0x%llx",
                sass_len, regs, nparams, tg[0], tg[1], tg[2], grid[0], grid[1], grid[2],
                nparams ? (unsigned long long)params[0] : 0ull);
    return 0;
}

void nvmtl_vk_cmd_dispatch_threads(nvk_cmdbuf *c, nvk_pipeline *p, const uint32_t threads[3], const uint32_t tgIn[3])
{
    uint32_t tg[3], groups[3];
    for (int d = 0; d < 3; d++) { if (!tgIn[d] || !threads[d]) return; tg[d] = tgIn[d]; groups[d] = (threads[d] + tg[d] - 1) / tg[d]; }
    if (!(p = nvmtl_align_guard_pick(c, p))) { c->skipped++; return; }
    VkDescriptorSet s[3] = { nvmtl_snapshot_set_c(c, 0, p), nvmtl_snapshot_set_c(c, 1, p), g_heapSet };
    if (!s[0] || !s[1]) { c->nobind = 1; return; }
    c->nobind = 0;
    int bound = 0;
    for (uint32_t mask = 0; mask < 8; mask++) {
        uint32_t local[3], gc[3], tbase[3] = { 0, 0, 0 }, gbase[3] = { 0, 0, 0 }; int nonempty = 1;
        for (int d = 0; d < 3; d++) {
            uint32_t full = threads[d] / tg[d], tail = threads[d] % tg[d];
            local[d] = tg[d]; gc[d] = 0;
            if (!(mask & (1u << d))) gc[d] = full;
            else if (!tail) nonempty = 0;
            else { local[d] = tail; gc[d] = 1; tbase[d] = full * tg[d]; gbase[d] = full; }
            if (!gc[d]) nonempty = 0;
        }
        if (!nonempty) continue;
        VkPipeline vp = nvmtl_compute_variant(p, local);
        if (!vp) { c->skipped++; continue; }
        pvkCmdBindPipeline(c->cb, VK_PIPELINE_BIND_POINT_COMPUTE, vp);
        if (!bound) { pvkCmdBindDescriptorSets(c->cb, VK_PIPELINE_BIND_POINT_COMPUTE, (VkPipelineLayout)p->layout, 0, g_heapSet ? 3 : 2, s, nvmtl_table_ubo() ? 2 : 0, g_dyn_zero); bound = 1; }
        // Metal allows any group count per axis; Vulkan caps it (NVK: 0x7fffffff x 65535 x 65535, the width of the QMD
        // grid fields). Geekbench's OpenCL Face Detection dispatched past 65535 on y and NAK's nak_fill_qmd aborted the
        // process. Split into chunks and advance thread_base/threadgroup_base, which the translated kernel adds to
        // GlobalInvocationId/WorkgroupId, so every chunk sees the positions of the whole grid.
        uint32_t cap[3];
        for (int d = 0; d < 3; d++) cap[d] = g_lim.maxComputeWorkGroupCount[d] ? g_lim.maxComputeWorkGroupCount[d] : 65535;
        if ((gc[0] > cap[0] || gc[1] > cap[1] || gc[2] > cap[2])) {
            static int said;
            if (!said++) nvlog("vk: dispatch of %ux%ux%u groups exceeds the device's %ux%ux%u — split into chunks",
                               gc[0], gc[1], gc[2], cap[0], cap[1], cap[2]);
        }
        for (uint32_t z = 0; z < gc[2]; z += cap[2])
        for (uint32_t y = 0; y < gc[1]; y += cap[1])
        for (uint32_t x = 0; x < gc[0]; x += cap[0]) {
            uint32_t off[3] = { x, y, z }, n[3];
            for (int d = 0; d < 3; d++) n[d] = gc[d] - off[d] < cap[d] ? gc[d] - off[d] : cap[d];
            uint32_t pc[12] = { threads[0], threads[1], threads[2],
                                tbase[0] + off[0] * local[0], tbase[1] + off[1] * local[1], tbase[2] + off[2] * local[2],
                                gbase[0] + off[0], gbase[1] + off[1], gbase[2] + off[2], groups[0], groups[1], groups[2] };
            pvkCmdPushConstants(c->cb, (VkPipelineLayout)p->layout, VK_SHADER_STAGE_COMPUTE_BIT, 0, NVMTL_DISPATCH_PC_BYTES, pc);
            pvkCmdDispatch(c->cb, n[0], n[1], n[2]);
        }
    }
}
void nvmtl_vk_cmd_dispatch_indirect_tg(nvk_cmdbuf *c, nvk_pipeline *p, nvk_buffer *b, size_t off, const uint32_t tgIn[3])
{
    uint32_t tg[3], g[3] = { 0, 0, 0 };
    for (int d = 0; d < 3; d++) { if (!tgIn[d]) return; tg[d] = tgIn[d]; }
    if (!(p = nvmtl_align_guard_pick(c, p))) { c->skipped++; return; }
    VkPipeline vp = nvmtl_compute_variant(p, tg);
    if (!vp) { c->skipped++; return; }
    VkDescriptorSet s[3] = { nvmtl_snapshot_set_c(c, 0, p), nvmtl_snapshot_set_c(c, 1, p), g_heapSet };
    if (!s[0] || !s[1]) { c->nobind = 1; return; }
    c->nobind = 0;
    if (b->map && off + 12 <= b->size) memcpy(g, (const char *)b->map + off, 12);
    else { static int said; if (!said++) nvlog("vk: indirect dispatch grid is not CPU-visible — a kernel asking [[threads_per_grid]] reads 0"); }
    pvkCmdBindPipeline(c->cb, VK_PIPELINE_BIND_POINT_COMPUTE, vp);
    pvkCmdBindDescriptorSets(c->cb, VK_PIPELINE_BIND_POINT_COMPUTE, (VkPipelineLayout)p->layout, 0, g_heapSet ? 3 : 2, s, nvmtl_table_ubo() ? 2 : 0, g_dyn_zero);
    uint32_t pc[12] = { g[0] * tg[0], g[1] * tg[1], g[2] * tg[2], 0, 0, 0, 0, 0, 0, g[0], g[1], g[2] };
    pvkCmdPushConstants(c->cb, (VkPipelineLayout)p->layout, VK_SHADER_STAGE_COMPUTE_BIT, 0, NVMTL_DISPATCH_PC_BYTES, pc);
    pvkCmdDispatchIndirect(c->cb, (VkBuffer)b->buf, off);
}
void nvmtl_vk_cmd_dispatch(nvk_cmdbuf *c, nvk_pipeline *p, uint32_t gx, uint32_t gy, uint32_t gz)
{
    pvkCmdBindPipeline(c->cb, VK_PIPELINE_BIND_POINT_COMPUTE, (VkPipeline)p->pipe);
    VkDescriptorSet s[3] = { nvmtl_snapshot_set_t(c, 0, p->stex_hi[0], p->tbany ? p->tbm[0] : NULL),
                             nvmtl_snapshot_set_t(c, 1, p->stex_hi[1], p->tbany ? p->tbm[1] : NULL), g_heapSet };
    if (!s[0] || !s[1]) { c->nobind = 1; return; }
    c->nobind = 0;
    pvkCmdBindDescriptorSets(c->cb, VK_PIPELINE_BIND_POINT_COMPUTE, (VkPipelineLayout)p->layout, 0, g_heapSet ? 3 : 2, s, nvmtl_table_ubo() ? 2 : 0, g_dyn_zero);
    // This path reads WorkgroupId raw (no base push constants), so it cannot be chunked. Past the device's group
    // count NAK aborts the whole app inside nak_fill_qmd; refuse the one dispatch loudly instead.
    if ((g_lim.maxComputeWorkGroupCount[1] && gy > g_lim.maxComputeWorkGroupCount[1]) ||
        (g_lim.maxComputeWorkGroupCount[2] && gz > g_lim.maxComputeWorkGroupCount[2]) ||
        (g_lim.maxComputeWorkGroupCount[0] && gx > g_lim.maxComputeWorkGroupCount[0])) {
        nvlog("vk: raw dispatch of %ux%ux%u groups exceeds the device limit — SKIPPED", gx, gy, gz);
        c->skipped++; return;
    }
    pvkCmdDispatch(c->cb, gx, gy, gz);
}

int nvmtl_vk_queue_create(nvk_queue *out)
{
    if (nvmtl_vk_init()) return -1;
    memset(out, 0, sizeof *out);
    VkCommandPoolCreateInfo cpci = { VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, NULL,
        VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT, g_qfam };
    VKCK(nvmtl_opaque_CreateCommandPool(g_dev, &cpci, NULL, &out->pool), "vkCreateCommandPool");
    return 0;
}

static long double nvmtl_tick_delta(uint64_t value, uint64_t anchor, unsigned bits)
{
    uint64_t d = value - anchor;
    if (bits < 64) d &= (1ull << bits) - 1;
    if (d & (1ull << (bits - 1))) {
        uint64_t magnitude = bits == 64 ? (~d + 1) : ((1ull << bits) - d);
        return -(long double)magnitude;
    }
    return (long double)d;
}
void nvmtl_vk_clock_refresh(void)
{
    if (g_state != 1 || !pvkGetCalibratedTimestampsEXT || !g_timestamp_bits) return;
    pthread_mutex_lock(&g_gpu_clock_lock);
    uint64_t now = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
    if (g_gpu_clock.valid && now >= g_gpu_clock.uptime_ns &&
        now - g_gpu_clock.uptime_ns < 1000000000ull) {
        pthread_mutex_unlock(&g_gpu_clock_lock); return;
    }
    nvmtl_gpu_clock best = {0}, previous = {0};
    unsigned advancing = 0;
    VkCalibratedTimestampInfoEXT info = { VK_STRUCTURE_TYPE_CALIBRATED_TIMESTAMP_INFO_EXT, NULL, VK_TIME_DOMAIN_DEVICE_EXT };
    for (unsigned i = 0; i < 3; ++i) {
        uint64_t tick = 0, deviation = UINT64_MAX;
        uint64_t before = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
        VkResult rc = pvkGetCalibratedTimestampsEXT(g_dev, 1, &info, &tick, &deviation);
        uint64_t after = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
        if (rc != VK_SUCCESS || after < before || after - before > 2000000ull || deviation > 1000000ull) break;
        uint64_t error = (after - before + 1) / 2 + deviation;
        if (error > 1000000ull) break;
        nvmtl_gpu_clock sample = { tick, before + (after-before)/2, error,
                                  g_timestamp_bits, g_lim.timestampPeriod, 1 };
        if (previous.valid) {
            long double elapsed = nvmtl_tick_delta(tick, previous.gpu_tick, g_timestamp_bits) * sample.period_ns;
            long double host = (long double)sample.uptime_ns - previous.uptime_ns;
            long double tolerance = (long double)error + previous.uncertainty_ns + 2000;
            if (elapsed <= 0 || fabsl(elapsed - host) > tolerance) break;
            ++advancing;
        }
        previous = sample;
        if (!best.valid || error < best.uncertainty_ns) best = sample;
    }
    g_gpu_clock = advancing == 2 ? best : (nvmtl_gpu_clock){0};
    pthread_mutex_unlock(&g_gpu_clock_lock);
}
static nvmtl_gpu_clock nvmtl_clock_snapshot(void)
{
    pthread_mutex_lock(&g_gpu_clock_lock);
    nvmtl_gpu_clock clock = g_gpu_clock;
    pthread_mutex_unlock(&g_gpu_clock_lock);
    return clock;
}
int nvmtl_vk_interval_seconds(const nvmtl_gpu_interval *t, double *start, double *end)
{
    if (start) *start = 0;
    if (end) *end = 0;
    if (!t || !t->valid || !t->clock.valid || !t->clock.valid_bits || t->clock.valid_bits > 64) return -1;
    long double a = (long double)t->clock.uptime_ns +
        nvmtl_tick_delta(t->start_tick, t->clock.gpu_tick, t->clock.valid_bits) * t->clock.period_ns;
    long double b = (long double)t->clock.uptime_ns +
        nvmtl_tick_delta(t->end_tick, t->clock.gpu_tick, t->clock.valid_bits) * t->clock.period_ns;
    long double error = t->clock.uncertainty_ns;
    if (!isfinite(a) || !isfinite(b) || a <= 0 || b < a) return -1;
    const long double lo = (long double)t->host_before_ns - error, hi = (long double)t->host_after_ns + error, dur = b - a;
    if (a < lo || b > hi) {
        if (dur > hi - lo) return -1;
        if (b > hi) { b = hi; a = b - dur; }
        if (a < lo) { a = lo; b = a + dur; }
    }
    if (start) *start = (double)(a / 1e9L);
    if (end) *end = (double)(b / 1e9L);
    return 0;
}
#define NVMTL_TSPOOL_MAX 256
static VkQueryPool g_tspool[NVMTL_TSPOOL_MAX]; static unsigned g_ntspool; static pthread_mutex_t g_tspool_lock = PTHREAD_MUTEX_INITIALIZER;
static unsigned nvmtl_tspool_cap(void) {
    static int cap = -1;
    if (cap < 0) { const char *e = getenv("NVMTL_TSPOOL_KEEP"); int v = e ? atoi(e) : NVMTL_TSPOOL_MAX;
        cap = v < 0 ? 0 : v > NVMTL_TSPOOL_MAX ? NVMTL_TSPOOL_MAX : v;
        nvlog("vk: timing query pools recycled (keep <= %d, NVMTL_TSPOOL_KEEP=%s) - one mapped allocation per pool, not per command buffer", cap, e ? e : "unset"); }
    return (unsigned)cap;
}
static VkQueryPool nvmtl_tspool_pop(void) {
    VkQueryPool q = VK_NULL_HANDLE;
    if (!nvmtl_tspool_cap()) return q;
    pthread_mutex_lock(&g_tspool_lock); if (g_ntspool) q = g_tspool[--g_ntspool]; pthread_mutex_unlock(&g_tspool_lock);
    return q;
}
static void nvmtl_tspool_push(VkQueryPool q) {
    int kept = 0;
    pthread_mutex_lock(&g_tspool_lock);
    if (g_ntspool < nvmtl_tspool_cap()) { g_tspool[g_ntspool++] = q; kept = 1; }
    pthread_mutex_unlock(&g_tspool_lock);
    if (!kept) pvkDestroyQueryPool(g_dev, q, NULL);
}
int nvmtl_vk_cmd_begin(nvk_queue *q, nvk_cmdbuf *out)
{
    (void)q;
    if (nvmtl_vk_init()) return -1;
    const uint64_t b0 = nvmtl_perf_now();
    memset(out, 0, sizeof *out);
    VkCommandPool pool = nvmtl_keep_pool_pop();
    VkCommandPoolCreateInfo cpci = { VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, NULL,
        VK_COMMAND_POOL_CREATE_TRANSIENT_BIT, g_qfam };
    VkResult result = pool != VK_NULL_HANDLE ? VK_SUCCESS : pvkCreateCommandPool(g_dev, &cpci, NULL, &pool);
    if (result != VK_SUCCESS) { nvlog("vk: recording pool creation -> %d", result); return -1; }
    out->pool = pool;
    VkCommandBufferAllocateInfo ai = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, NULL, pool,
        VK_COMMAND_BUFFER_LEVEL_PRIMARY, 1 };
    result = nvmtl_opaque_AllocateCommandBuffers(g_dev, &ai, &out->cb);
    if (result != VK_SUCCESS) { pvkDestroyCommandPool(g_dev, pool, NULL); memset(out, 0, sizeof *out); return -1; }
    VkCommandBufferBeginInfo bi = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, NULL,
        VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT, NULL };
    result = pvkBeginCommandBuffer(out->cb, &bi);
    if (result != VK_SUCCESS) { pvkDestroyCommandPool(g_dev, pool, NULL); memset(out, 0, sizeof *out); return -1; }
    if (pvkGetCalibratedTimestampsEXT && g_timestamp_bits) {
        VkQueryPoolCreateInfo qi = { VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO, NULL, 0, VK_QUERY_TYPE_TIMESTAMP, 2, 0 };
        VkQueryPool timing = nvmtl_tspool_pop();
        if (timing != VK_NULL_HANDLE || pvkCreateQueryPool(g_dev, &qi, NULL, &timing) == VK_SUCCESS) {
            out->timing_pool = timing;
            pvkCmdResetQueryPool(out->cb, timing, 0, 2);
            pvkCmdWriteTimestamp(out->cb, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, timing, 0);
        }
    }
    g_cmd_begun++;
    nvmtl_perf_note(NVP_BEGIN, b0, 0);
    return 0;
}

static pthread_mutex_t g_rpv_lock = PTHREAD_MUTEX_INITIALIZER;
static void *nvmtl_rp_variant(nvk_pipeline *p, uint32_t load, uint32_t was)
{
    uint32_t cf[NVMTL_NCOL], rmask, ncol = nvmtl_rt_shape(p, cf, &rmask), keep = 0;
    for (uint32_t i = 0; i < ncol; i++) if (cf[i]) keep |= NVMTL_LOAD_COLOUR(i);
    if (p->has_depth) keep |= NVMTL_LOAD_DEPTH | (p->has_stencil ? NVMTL_LOAD_STENCIL : 0u);
    load &= keep;
    {
        static uint32_t seenmrt, seenany;
        if (ncol >= 2 && !(__atomic_fetch_or(&seenmrt, 1u << ncol, __ATOMIC_RELAXED) & (1u << ncol)))
            nvlog("vk: apps2 MRT PASS: %u colour slots (load 0x%x of 0x%x possible, depth %d, stencil %u, samples %u, resolve 0x%x)",
                  ncol, load, keep, p->has_depth, p->has_stencil, p->samples, rmask);
        if (!__atomic_fetch_or(&seenany, 1u, __ATOMIC_RELAXED))
            nvlog("vk: apps2 pass-shape instrument LIVE (first pass: %u colour slot(s), depth %d) — a run with no "
                  "'MRT PASS' line above therefore MEASURED that every render pass in this process had one colour slot",
                  ncol, p->has_depth);
    }
    if (!load) return p->rp;
    pthread_mutex_lock(&g_rpv_lock);
    int legacy = load < 4u; void *rp = legacy ? p->rpv[load] : NULL;
    for (uint32_t i = 0; !legacy && i < p->nrpx; i++) if (p->rpxk[i] == load) { rp = p->rpx[i]; break; }
    if (!rp && !legacy && p->nrpx >= p->crpx && p->crpx < NVMTL_RPX_MAX) {
        uint32_t cap = p->crpx ? p->crpx * 2u : NVMTL_RPX_GROW;
        if (cap > NVMTL_RPX_MAX) cap = NVMTL_RPX_MAX;
        void **gx = (void **)realloc(p->rpx, cap * sizeof *gx);
        if (gx) { p->rpx = gx; uint32_t *gk = (uint32_t *)realloc(p->rpxk, cap * sizeof *gk); if (gk) { p->rpxk = gk; p->crpx = cap; } }
    }
    if (!rp && !legacy && p->nrpx >= p->crpx)
        nvlog("vk: apps2 load variant 0x%x on pipeline %p: rpx holds %u of a DERIVED ceiling of %u and did not grow — "
              "THIS PASS CLEARS INSTEAD OF LOADING and the attachment's previous contents are LOST",
              load, (void *)p, p->crpx, NVMTL_RPX_MAX);
    else if (!rp) {
        VkRenderPass made = VK_NULL_HANDLE;
        VkResult r = nvmtl_make_render_pass(p, load, &made);
        if (r == VK_SUCCESS && legacy) { rp = p->rpv[load] = (void *)made; nvlog("vk: G11 load-action render pass variant %u made (%u colour slot(s), colour %s, depth %s)", load, ncol, (load & 1u) ? "LOAD" : "clear", (load & 2u) ? "LOAD" : "clear"); }
        else if (r == VK_SUCCESS) { rp = (void *)made; p->rpxk[p->nrpx] = load; p->rpx[p->nrpx++] = rp;
            nvlog("vk: apps2 load variant 0x%x made (%u colour slot(s), stencil %s)", load, ncol, (load & NVMTL_LOAD_STENCIL) ? "LOAD" : "clear"); }
        else if (legacy) nvlog("vk: G11 vkCreateRenderPass(variant %u) -> %d — this pass CLEARS instead of loading", load, r);
        else nvlog("vk: apps2 vkCreateRenderPass(variant 0x%x) -> %d — this pass CLEARS instead of loading", load, r);
    }
    pthread_mutex_unlock(&g_rpv_lock);
    if (rp && (load & 1u) && was != VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL && was != VK_IMAGE_LAYOUT_UNDEFINED) {
        static int said; if (!said++) nvlog("vk: G11 a Load pass began on an image resting in layout %u (declared TRANSFER_SRC)", was);
    }
    return rp;
}

int nvmtl_vk_cmd_begin_render_ex(nvk_cmdbuf *c, nvk_image *img, nvk_pipeline *p, const float clear[4], uint32_t load)
{ return nvmtl_vk_cmd_begin_render_ms(c, img, p, clear, load, NULL); }
static void nvmtl_clear_colour(VkClearColorValue *cv, uint32_t fmt, const float c[4])
{
    int kind = 0; double lo = 0, hi[4] = { 0, 0, 0, 0 }, m = 0;
    switch (fmt) {
        case VK_FORMAT_R8_UINT: case VK_FORMAT_R8G8_UINT: case VK_FORMAT_R8G8B8A8_UINT: case VK_FORMAT_B8G8R8A8_UINT: kind = 1; m = 255; break;
        case VK_FORMAT_R16_UINT: case VK_FORMAT_R16G16_UINT: case VK_FORMAT_R16G16B16A16_UINT: kind = 1; m = 65535; break;
        case VK_FORMAT_R32_UINT: case VK_FORMAT_R32G32_UINT: case VK_FORMAT_R32G32B32A32_UINT: kind = 1; m = 4294967295.0; break;
        case VK_FORMAT_A2B10G10R10_UINT_PACK32: case VK_FORMAT_A2R10G10B10_UINT_PACK32: kind = 1; m = -1; break;
        case VK_FORMAT_R8_SINT: case VK_FORMAT_R8G8_SINT: case VK_FORMAT_R8G8B8A8_SINT: kind = 2; m = 127; break;
        case VK_FORMAT_R16_SINT: case VK_FORMAT_R16G16_SINT: case VK_FORMAT_R16G16B16A16_SINT: kind = 2; m = 32767; break;
        case VK_FORMAT_R32_SINT: case VK_FORMAT_R32G32_SINT: case VK_FORMAT_R32G32B32A32_SINT: kind = 2; m = 2147483647.0; break;
        default: memcpy(cv->float32, c, 4 * sizeof(float)); return;
    }
    for (int i = 0; i < 4; i++) hi[i] = m < 0 ? (i == 3 ? 3 : 1023) : m;
    for (int i = 0; i < 4; i++) {
        double v = c[i];
        if (kind == 1) { lo = 0; v = v < lo ? lo : v > hi[i] ? hi[i] : v; cv->uint32[i] = (uint32_t)v; }
        else { lo = -hi[i] - 1; v = v < lo ? lo : v > hi[i] ? hi[i] : v; cv->int32[i] = (int32_t)v; }
    }
}

static void nvmtl_private_depth_layout(nvk_cmdbuf *, nvk_image *, VkImageLayout);
int nvmtl_vk_cmd_begin_pass(nvk_cmdbuf *c, const nvk_pass *ps, nvk_pipeline *p)
{
    if (!p->rp || !ps->w || !ps->h) { nvlog("begin_render: apps2 no render pass or a %ux%u pass — pass REFUSED", ps->w, ps->h); return -1; }
    nvmtl_sample_pattern pattern = ps->sample_pattern;
    if (!pattern.count && nvmtl_sample_default(p->samples > 1 ? p->samples : 1, &pattern)) return -1;
    if (!nvmtl_sample_pattern_valid(&pattern) || pattern.count != (p->samples > 1 ? p->samples : 1)) {
        nvlog("sample positions: count/pattern incompatible with render pipeline"); return -1;
    }
    if (ps->sample_pattern.count && !g_sample_locations) { nvlog("sample positions: backend unavailable"); return -1; }
    if (nvmtl_sample_pattern_quantize(&pattern, &pattern)) return -1;
    uint32_t cf[NVMTL_NCOL], rmask, ncol = nvmtl_rt_shape(p, cf, &rmask);
    for (uint32_t i = 0; i < ncol; i++) {
        if (!cf[i]) continue;
        nvk_image *img = ps->col[i], *resolve = ps->res[i];
        if (!img || !ps->colview[i]) { nvlog("begin_render: apps2 the pipeline draws colour attachment %u and the pass has no texture there — pass REFUSED", i); return -1; }
        if (p->samples > 1) {
            if (img->samples != p->samples) { nvlog("begin_render: pipeline has %u samples but the colour target has %u — pass REFUSED", p->samples, img->samples); return -1; }
            if ((rmask & (1u << i)) && resolve && (resolve->samples > 1 || ps->resw[i] != ps->colw[i] || ps->resh[i] != ps->colh[i])) { nvlog("begin_render: resolve texture must be 1-sample and the same size — pass REFUSED"); return -1; }
        } else if (img->samples > 1) { nvlog("begin_render: a 1-sample pipeline on a %u-sample target — pass REFUSED", img->samples); return -1; }
        if ((ps->colfmt[i] ? ps->colfmt[i] : img->fmt) != cf[i]) { static int said; if (said++ < 4) nvlog("begin_render: apps2 colour %u: the pipeline's format %u draws into a format-%u image (census: the image format; a format-view texture attaches in its view format since gl3)", i, cf[i], img->fmt); }
    }
    if (p->has_depth && !ps->dsview) { nvlog("begin_render: pipeline has a depth attachment but the encoder has no depth texture — pass REFUSED"); return -1; }
    if (p->has_depth && ps->ds) {
        uint32_t ds_s = ps->ds->samples > 1 ? ps->ds->samples : 1u, p_s = p->samples > 1 ? p->samples : 1u;
        if (ds_s != p_s) { nvlog("begin_render: apps2 pipeline has %u samples but the depth target has %u — pass REFUSED", p_s, ds_s); return -1; }
        if (ps->ds->fmt != p->dfmt) { static int said; if (said++ < 4) nvlog("begin_render: apps2 depth: the pipeline's format %u on a format-%u texture (census)", p->dfmt, ps->ds->fmt); }
    }
    nvk_pass full;
    {
        uint32_t add = 0;
        const uint32_t nl = ps->layers > 1 ? ps->layers : 1u;
        for (uint32_t i = 0; i < ncol; i++) {
            nvk_image *img = ps->col[i];
            if (!cf[i] || !img || (ps->load & NVMTL_LOAD_COLOUR(i)) || (ps->colw[i] <= ps->w && ps->colh[i] <= ps->h)) continue;
            if (c->in_rp || img->mtl_type == 7 || (ps->colfmt[i] && ps->colfmt[i] != img->fmt)) {
                static int said; if (said++ < 4) nvlog("begin_render: fullclear colour %u (%ux%u) is larger than the %ux%u render area but is a %s — only the area is cleared",
                    i, ps->colw[i], ps->colh[i], ps->w, ps->h, c->in_rp ? "pass in progress" : img->mtl_type == 7 ? "3D slice" : "format view");
                continue;
            }
            VkImageSubresourceRange r = { VK_IMAGE_ASPECT_COLOR_BIT, ps->col_level[i], 1, ps->col_layer[i], nl };
            VkImageMemoryBarrier b0 = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_TRANSFER_WRITE_BIT,
                VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)img->img, r };
            pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &b0);
            VkClearColorValue cv; nvmtl_clear_colour(&cv, img->fmt, ps->clear[i]);
            pvkCmdClearColorImage(c->cb, (VkImage)img->img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &cv, 1, &r);
            VkImageMemoryBarrier b1 = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, VK_ACCESS_TRANSFER_WRITE_BIT,
                VK_ACCESS_COLOR_ATTACHMENT_READ_BIT | VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT,
                VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)img->img, r };
            pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, 0, 0, NULL, 0, NULL, 1, &b1);
            img->layout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
            add |= NVMTL_LOAD_COLOUR(i);
        }
        if (p->has_depth && ps->ds && !c->in_rp) {
            nvk_image *d = ps->ds;
            const uint32_t dw = (d->w >> ps->ds_level) ? (d->w >> ps->ds_level) : 1u, dh = (d->h >> ps->ds_level) ? (d->h >> ps->ds_level) : 1u;
            const int dl = (ps->load & NVMTL_LOAD_DEPTH) != 0, sl = p->has_stencil ? (ps->load & NVMTL_LOAD_STENCIL) != 0 : dl;
            VkImageAspectFlags all = nvmtl_copy_aspect(d), clr = 0;
            if (d->fmt == VK_FORMAT_D16_UNORM_S8_UINT || d->fmt == VK_FORMAT_D24_UNORM_S8_UINT || d->fmt == VK_FORMAT_D32_SFLOAT_S8_UINT)
                all |= VK_IMAGE_ASPECT_STENCIL_BIT;
            if (!dl && (all & VK_IMAGE_ASPECT_DEPTH_BIT)) clr |= VK_IMAGE_ASPECT_DEPTH_BIT;
            if (p->has_stencil && !sl && (all & VK_IMAGE_ASPECT_STENCIL_BIT)) clr |= VK_IMAGE_ASPECT_STENCIL_BIT;
            if (clr && (dw > ps->w || dh > ps->h)) {
                VkImageSubresourceRange r = { all, ps->ds_level, 1, ps->ds_layer, nl };
                VkImageMemoryBarrier b0 = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_TRANSFER_WRITE_BIT,
                    clr == all ? VK_IMAGE_LAYOUT_UNDEFINED : (VkImageLayout)d->layout, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                    VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)d->img, r };
                pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &b0);
                VkClearDepthStencilValue dv = { ps->clear_depth, ps->clear_stencil };
                VkImageSubresourceRange rc = r; rc.aspectMask = clr;
                pvkCmdClearDepthStencilImage(c->cb, (VkImage)d->img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &dv, 1, &rc);
                VkImageMemoryBarrier b1 = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, VK_ACCESS_TRANSFER_WRITE_BIT,
                    VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_READ_BIT | VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
                    VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                    VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)d->img, r };
                pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_TRANSFER_BIT,
                    VK_PIPELINE_STAGE_EARLY_FRAGMENT_TESTS_BIT | VK_PIPELINE_STAGE_LATE_FRAGMENT_TESTS_BIT, 0, 0, NULL, 0, NULL, 1, &b1);
                d->layout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
                add |= ((clr & VK_IMAGE_ASPECT_DEPTH_BIT) ? NVMTL_LOAD_DEPTH : 0u) | ((clr & VK_IMAGE_ASPECT_STENCIL_BIT) ? NVMTL_LOAD_STENCIL : 0u);
                if (!p->has_stencil) add |= NVMTL_LOAD_STENCIL;
            }
        }
        if (add) {
            static int said; if (said++ < 8) nvlog("begin_render: fullclear %ux%u render area — Clear attachments 0x%x are larger and were cleared WHOLE, as Apple and AMD do",
                                                   ps->w, ps->h, add);
            full = *ps; full.load |= add; ps = &full;
        }
    }
    for (uint32_t i = 0; i < NVMTL_NCOL; i++) {
        nvk_image *img = ps->col[i];
        if (!img || (i < ncol && cf[i]) || (ps->load & NVMTL_LOAD_COLOUR(i))) continue;
        if (ps->colview[i] == img->view && img->samples <= 1) { if (nvmtl_vk_cmd_clear_image(c, img, ps->clear[i])) nvlog("begin_render: apps2 colour %u clear FAILED", i); }
        else { static int said; if (said++ < 4) nvlog("begin_render: apps2 colour %u is not drawn by this pipeline and is a level/slice/multisample view — its Clear is NOT applied", i); }
    }
    if (p->has_depth && ps->ds && g_sample_locations &&
        (ps->load & (NVMTL_LOAD_DEPTH | NVMTL_LOAD_STENCIL)))
        nvmtl_private_depth_layout(c, ps->ds, VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL);
    VkImageView views[2 * NVMTL_NCOL + 2]; uint32_t nviews = 0, was = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL; int first = 1;
    uint32_t prm = 0;
    for (uint32_t i = 0; p->samples > 1 && i < ncol; i++) if (cf[i] && (rmask & (1u << i)) && ps->res[i] && ps->resview[i]) prm |= 1u << i;
    const int dres = p->samples > 1 && p->has_depth && ps->dres && ps->dresview && (ps->dresmode || ps->sresmode);
    void *rpShape = NULL;
    if (p->samples > 1 && (prm != (rmask & ((1u << ncol) - 1u)) || dres)) {
        rpShape = nvmtl_rp_shape_variant(p, ps->load, prm, dres ? ps->dresmode : 0, dres ? ps->sresmode : 0);
        if (!rpShape) { nvlog("begin_render: par1 no resolve-shape render pass — pass REFUSED"); return -1; }
        static int said; if (said++ < 8) nvlog("begin_render: par1 %u-sample pass resolves colour 0x%x of 0x%x%s", p->samples, prm, rmask, dres ? " + depth/stencil" : "");
    }
    for (uint32_t i = 0; i < ncol; i++) {
        if (!cf[i]) continue;
        if (first) { was = ps->col[i]->layout; first = 0; }
        ps->col[i]->layout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
        views[nviews++] = (VkImageView)ps->colview[i];
    }
    if (p->has_depth) { views[nviews++] = (VkImageView)ps->dsview; if (ps->ds) ps->ds->layout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL; }
    for (uint32_t i = 0; p->samples > 1 && i < ncol; i++) {
        if (!cf[i] || !(prm & (1u << i))) continue;
        views[nviews++] = (VkImageView)ps->resview[i]; ps->res[i]->layout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
    }
    if (dres) { views[nviews++] = (VkImageView)ps->dresview; if (ps->dres) ps->dres->layout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL; }
    VkFramebufferCreateInfo fci = { VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, NULL, 0, rpShape ? (VkRenderPass)rpShape : p->rp,
        nviews, views, ps->w, ps->h, ps->layers > 1 ? ps->layers : 1 };
    VkFramebuffer fb = VK_NULL_HANDLE;
    VKCK(pvkCreateFramebuffer(g_dev, &fci, NULL, &fb), "vkCreateFramebuffer");
    c->fb = fb;
    if (c->nfb == c->capfb) {
        unsigned cap = c->capfb ? c->capfb * 2 : 16;
        void **grown = (void **)realloc(c->fbs, cap * sizeof *grown);
        if (grown) { c->fbs = grown; c->capfb = cap; }
    }
    if (c->nfb < c->capfb) c->fbs[c->nfb++] = fb;
    else {

        pvkDestroyFramebuffer(g_dev, fb, NULL); c->fb = NULL;
        nvlog("vk: framebuffer retention allocation failed; pass refused");
        return -1;
    }
    VkClearValue cv[2 * NVMTL_NCOL + 1]; uint32_t ncv = 0; memset(cv, 0, sizeof cv);
    for (uint32_t i = 0; i < ncol; i++) if (cf[i]) { nvmtl_clear_colour(&cv[ncv].color, cf[i], ps->clear[i]); ncv++; }
    if (p->has_depth) { cv[ncv].depthStencil.depth = ps->clear_depth; cv[ncv].depthStencil.stencil = ps->clear_stencil; ncv++; }
    void *rpUse = rpShape ? rpShape : nvmtl_rp_variant(p, ps->load, was);
    if (!rpUse) rpUse = p->rp;
    VkRenderPassBeginInfo rbi = { VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, NULL, (VkRenderPass)rpUse, c->fb,
        { { 0, 0 }, { ps->w, ps->h } }, ncv, cv };
    c->sample_pattern = pattern;
    VkSampleLocationEXT vkpos[8];
    VkSampleLocationsInfoEXT locations = nvmtl_sample_info(&pattern, vkpos);
    VkAttachmentSampleLocationsEXT attachment = { p->has_depth ? ncv - 1 : 0, locations };
    VkSubpassSampleLocationsEXT subpass = { 0, locations };
    VkRenderPassSampleLocationsBeginInfoEXT location_begin = {
        VK_STRUCTURE_TYPE_RENDER_PASS_SAMPLE_LOCATIONS_BEGIN_INFO_EXT, NULL,
        p->has_depth ? 1u : 0u, p->has_depth ? &attachment : NULL,
        p->has_depth ? 1u : 0u, p->has_depth ? &subpass : NULL };
    if (g_sample_locations) {

        rbi.pNext = &location_begin;
        pvkCmdSetSampleLocationsEXT(c->cb, &locations);
    }
    { extern int nvmtl_pt_armed(void);
      if (nvmtl_pt_armed() && c->pt_n < 128 && g_timestamp_bits) {
          if (!c->pt_pool) { VkQueryPoolCreateInfo qi = { VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO, NULL, 0, VK_QUERY_TYPE_TIMESTAMP, 256, 0 };
              VkQueryPool qp; if (pvkCreateQueryPool(g_dev, &qi, NULL, &qp) == VK_SUCCESS) { pvkCmdResetQueryPool(c->cb, qp, 0, 256); c->pt_pool = (void *)qp; } }
          if (c->pt_pool) { pvkCmdWriteTimestamp(c->cb, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, (VkQueryPool)c->pt_pool, 2 * c->pt_n);
              c->pt_w[c->pt_n] = ps->w; c->pt_h[c->pt_n] = ps->h; c->pt_f[c->pt_n] = ncol ? cf[0] : (p->has_depth ? 1000u + p->dfmt : 0u); c->pt_open = 1;
              memset(c->pt_fs[c->pt_n], 0, sizeof c->pt_fs[c->pt_n]); }
      } }
    pvkCmdBeginRenderPass(c->cb, &rbi, VK_SUBPASS_CONTENTS_INLINE);
    c->in_rp = 1;
    if (p->pipe) pvkCmdBindPipeline(c->cb, VK_PIPELINE_BIND_POINT_GRAPHICS, p->pipe);
    VkViewport vp = { 0, (float)ps->h, (float)ps->w, -(float)ps->h, 0, 1 }; VkRect2D sc = { { 0, 0 }, { ps->w, ps->h } };
    pvkCmdSetViewport(c->cb, 0, 1, &vp); pvkCmdSetScissor(c->cb, 0, 1, &sc);
    return 0;
}
int nvmtl_vk_cmd_begin_render_ms(nvk_cmdbuf *c, nvk_image *img, nvk_pipeline *p, const float clear[4], uint32_t load, nvk_image *resolve)
{
    nvk_pass ps; memset(&ps, 0, sizeof ps);
    ps.col[0] = img; ps.colview[0] = img->view; ps.colw[0] = img->w; ps.colh[0] = img->h;
    if (resolve) { ps.res[0] = resolve; ps.resview[0] = resolve->view; ps.resw[0] = resolve->w; ps.resh[0] = resolve->h; }
    memcpy(ps.clear[0], clear, 4 * sizeof(float));
    ps.dsview = c->depth_view; ps.clear_depth = 1.0f;
    ps.load = load | ((load & NVMTL_LOAD_DEPTH) ? NVMTL_LOAD_STENCIL : 0u);
    ps.w = img->w; ps.h = img->h;
    return nvmtl_vk_cmd_begin_pass(c, &ps, p);
}

int nvmtl_vk_cmd_retire_view(nvk_cmdbuf *c, void *view)
{
    if (c->nrv == c->caprv) {
        unsigned cap = c->caprv ? c->caprv * 2 : 8;
        void **grown = (void **)realloc(c->rvs, cap * sizeof *grown);
        if (grown) { c->rvs = grown; c->caprv = cap; }
    }
    if (c->nrv < c->caprv) { c->rvs[c->nrv++] = view; return 0; }
    nvlog("vk: apps2 attachment-view list realloc FAILED — view %p destroyed now and its pass REFUSED", view);
    pvkDestroyImageView(g_dev, (VkImageView)view, NULL);
    return -1;
}
int nvmtl_vk_cmd_attachment_view(nvk_cmdbuf *c, nvk_image *img, int depth, uint32_t level, uint32_t slice, void **out, uint32_t *w, uint32_t *h)
{
    const uint32_t mips = img->mips ? img->mips : 1, layers = img->layers ? img->layers : 1;
    *out = NULL;
    if (level >= mips || slice >= layers) { nvlog("encoder: apps2 attachment level %u slice %u is outside the image (%u level(s), %u slice(s)) — pass REFUSED", level, slice, mips, layers); return -1; }
    *w = img->w >> level ? img->w >> level : 1u; *h = img->h >> level ? img->h >> level : 1u;
    if ((mips <= 1 && layers <= 1) || ((depth || img->mtl_type == 7) && !level && !slice)) {
        if (!img->view) { nvlog("encoder: apps2 attachment image has no view — pass REFUSED"); return -1; }
        if (mips > 1 || layers > 1 || img->mtl_type == 7) { static int said; if (said++ < 4) nvlog("encoder: apps2 a %s attachment at level 0 slice 0 of a %u-level %u-slice image uses the image's own view (as before)", depth ? "depth/stencil" : "3D", mips, layers); }
        *out = img->view; return 0;
    }
    if (depth) { nvlog("encoder: apps2 a depth/stencil attachment at level %u slice %u is not carried — pass REFUSED", level, slice); return -1; }
    if (img->mtl_type == 7 && !(img->vkflags & VK_IMAGE_CREATE_2D_ARRAY_COMPATIBLE_BIT)) {
        nvlog("encoder: apps2 a 3D attachment at level %u slice %u needs 2D_ARRAY_COMPATIBLE, which this image did not get — pass REFUSED", level, slice);
        return -1; }
    if (img->mtl_type == 7 && mips > 1) {
        nvlog("encoder: apps2 a 3D attachment at level %u slice %u of a %u-level volume is not carried (a mip's depth is not `layers`) — pass REFUSED", level, slice, mips);
        return -1; }
    uint32_t vt = img->mtl_type <= 1 ? 0u : img->mtl_type == 4 ? 4u : 2u;
    void *v = NULL;
    if (nvmtl_vk_image_view_create_range(img, img->fmt, 0, level, 1, slice, 1, vt, &v)) return -1;
    if (nvmtl_vk_cmd_retire_view(c, v)) return -1;
    *out = v; return 0;
}
int nvmtl_vk_cmd_attachment_view_fmt(nvk_cmdbuf *c, nvk_image *img, int depth, uint32_t level, uint32_t slice, uint32_t vkfmt, int a8,
                                     void **out, uint32_t *w, uint32_t *h)
{
    if (depth || !vkfmt || vkfmt == img->fmt) return nvmtl_vk_cmd_attachment_view(c, img, depth, level, slice, out, w, h);
    const uint32_t mips = img->mips ? img->mips : 1, layers = img->layers ? img->layers : 1;
    *out = NULL;
    if (level >= mips || slice >= layers) { nvlog("encoder: gl3 format-view attachment level %u slice %u is outside the image (%u level(s), %u slice(s)) — pass REFUSED", level, slice, mips, layers); return -1; }
    if (img->mtl_type == 7) { nvlog("encoder: gl3 a format view of a 3D attachment is not carried — pass REFUSED"); return -1; }
    *w = img->w >> level ? img->w >> level : 1u; *h = img->h >> level ? img->h >> level : 1u;
    uint32_t vt = img->mtl_type <= 1 ? 0u : img->mtl_type == 4 ? 4u : 2u;
    void *v = NULL;
    if (nvmtl_vk_image_view_create_range(img, vkfmt, a8, level, 1, slice, 1, vt, &v)) return -1;
    if (nvmtl_vk_cmd_retire_view(c, v)) return -1;
    static int said; if (said++ < 4) nvlog("encoder: gl3 colour attachment in its VIEW's format %u over a format-%u image", vkfmt, img->fmt);
    *out = v; return 0;
}
void nvmtl_vk_pipeline_sig(const nvk_pipeline *p, nvk_rtsig *out)
{
    uint32_t rmask; memset(out, 0, sizeof *out);
    out->ncol = nvmtl_rt_shape(p, out->cfmts, &rmask);
    while (out->ncol && !out->cfmts[out->ncol - 1]) out->ncol--;
    out->has_depth = p->has_depth ? 1u : 0u; out->dfmt = p->has_depth ? p->dfmt : 0u; out->samples = p->samples > 1 ? p->samples : 1u;
}
int nvmtl_vk_pipeline_compatible(const nvk_rtsig *sig, const nvk_pipeline *p)
{ nvk_rtsig s; nvmtl_vk_pipeline_sig(p, &s); return !memcmp(&s, sig, sizeof s); }

static int nvmtl_fmt_stencil(uint32_t f) { return f >= VK_FORMAT_S8_UINT && f <= VK_FORMAT_D32_SFLOAT_S8_UINT; }
int nvmtl_vk_cmd_attachment_view_n(nvk_cmdbuf *c, nvk_image *img, int depth, uint32_t level, uint32_t slice, uint32_t nlayers,
                                   uint32_t vkfmt, int a8, void **out, uint32_t *w, uint32_t *h)
{
    if (nlayers <= 1 && !(depth && (level || slice || img->layers > 1 || img->mips > 1)))
        return nvmtl_vk_cmd_attachment_view_fmt(c, img, depth, level, slice, vkfmt, a8, out, w, h);
    if (!nlayers) nlayers = 1;
    const uint32_t mips = img->mips ? img->mips : 1, layers = img->layers ? img->layers : 1;
    *out = NULL;
    if (level >= mips || slice + nlayers > layers) {
        nvlog("encoder: par1 attachment level %u slices [%u,%u) is outside the image (%u level(s), %u slice(s)) — pass REFUSED", level, slice, slice + nlayers, mips, layers);
        return -1; }
    if (img->mtl_type == 7) { nvlog("encoder: par1 a %s 3D attachment is not carried — pass REFUSED", nlayers > 1 ? "layered" : "depth"); return -1; }
    *w = img->w >> level ? img->w >> level : 1u; *h = img->h >> level ? img->h >> level : 1u;
    void *v = NULL;
    if (depth) {
        VkImageAspectFlags asp = img->fmt == VK_FORMAT_S8_UINT ? VK_IMAGE_ASPECT_STENCIL_BIT
                               : (VkImageAspectFlags)(VK_IMAGE_ASPECT_DEPTH_BIT | (nvmtl_fmt_stencil(img->fmt) ? VK_IMAGE_ASPECT_STENCIL_BIT : 0));
        VkImageViewCreateInfo vci = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, (VkImage)img->img,
            nlayers > 1 ? VK_IMAGE_VIEW_TYPE_2D_ARRAY : VK_IMAGE_VIEW_TYPE_2D, (VkFormat)img->fmt, { 0 }, { asp, level, 1, slice, nlayers } };
        VkImageView vv = VK_NULL_HANDLE;
        VkResult r = pvkCreateImageView(g_dev, &vci, NULL, &vv);
        if (r != VK_SUCCESS) { nvlog("encoder: par1 depth attachment view level %u slices [%u,%u) -> %d — pass REFUSED", level, slice, slice + nlayers, r); return -1; }
        v = (void *)vv;
    } else {
        uint32_t vt = nlayers > 1 ? (img->mtl_type <= 1 ? 1u : 3u) : (img->mtl_type <= 1 ? 0u : img->mtl_type == 4 ? 4u : 2u);
        if (nvmtl_vk_image_view_create_range(img, vkfmt ? vkfmt : img->fmt, a8, level, 1, slice, nlayers, vt, &v)) return -1;
    }
    if (nvmtl_vk_cmd_retire_view(c, v)) return -1;
    *out = v; return 0;
}
static VkResolveModeFlagBits nvmtl_resolve_mode(uint32_t mtlp1, int stencil)
{
    if (!mtlp1) return VK_RESOLVE_MODE_NONE;
    uint32_t f = mtlp1 - 1;
    VkResolveModeFlagBits m = stencil ? VK_RESOLVE_MODE_SAMPLE_ZERO_BIT : f == 1 ? VK_RESOLVE_MODE_MIN_BIT : f == 2 ? VK_RESOLVE_MODE_MAX_BIT : VK_RESOLVE_MODE_SAMPLE_ZERO_BIT;
    if (stencil && f == 1) { static int said; if (!said++) nvlog("encoder: par1 MTLMultisampleStencilResolveFilterDepthResolvedSample has no Vulkan mode — stencil resolves sample 0"); }
    uint32_t have = stencil ? g_dsr_stencil_modes : g_dsr_depth_modes;
    if (!(have & m)) { static int said; if (said++ < 4) nvlog("encoder: par1 %s resolve mode 0x%x is not offered by this device (0x%x) — sample 0", stencil ? "stencil" : "depth", m, have); m = VK_RESOLVE_MODE_SAMPLE_ZERO_BIT; }
    return m;
}
static VkResult nvmtl_make_render_pass_shape(const nvk_pipeline *p, uint32_t load, uint32_t prm, uint32_t dmode, uint32_t smode, VkRenderPass *out)
{
    if (!pvkCreateRenderPass2) return VK_ERROR_FEATURE_NOT_PRESENT;
    VkSampleCountFlagBits sc = (VkSampleCountFlagBits)(p->samples > 1 ? p->samples : 1);
    int ms = p->samples > 1, anyres = 0;
    uint32_t cf[NVMTL_NCOL], rmask, ncol = nvmtl_rt_shape(p, cf, &rmask);
    VkAttachmentDescription2 atts[2 * NVMTL_NCOL + 2]; uint32_t n = 0;
    VkAttachmentReference2 cref[NVMTL_NCOL], rref[NVMTL_NCOL];
    VkAttachmentReference2 dref = { VK_STRUCTURE_TYPE_ATTACHMENT_REFERENCE_2, NULL, VK_ATTACHMENT_UNUSED, VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL, 0 };
    VkAttachmentReference2 drref = dref;
    for (uint32_t i = 0; i < ncol; i++) {
        cref[i] = rref[i] = (VkAttachmentReference2){ VK_STRUCTURE_TYPE_ATTACHMENT_REFERENCE_2, NULL, VK_ATTACHMENT_UNUSED, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, 0 };
        if (!cf[i]) continue;
        int ld = (load & NVMTL_LOAD_COLOUR(i)) != 0;
        cref[i].attachment = n;
        atts[n++] = (VkAttachmentDescription2){ VK_STRUCTURE_TYPE_ATTACHMENT_DESCRIPTION_2, NULL, 0, (VkFormat)cf[i], sc,
            ld ? VK_ATTACHMENT_LOAD_OP_LOAD : VK_ATTACHMENT_LOAD_OP_CLEAR, VK_ATTACHMENT_STORE_OP_STORE,
            VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_DONT_CARE,
            ld ? VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL : VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL };
    }
    if (p->has_depth) {
        int dl = (load & NVMTL_LOAD_DEPTH) != 0, sl = p->has_stencil ? (load & NVMTL_LOAD_STENCIL) != 0 : dl;
        dref.attachment = n;
        atts[n++] = (VkAttachmentDescription2){ VK_STRUCTURE_TYPE_ATTACHMENT_DESCRIPTION_2, NULL, 0, (VkFormat)(p->dfmt ? p->dfmt : VK_FORMAT_D32_SFLOAT), sc,
            dl ? VK_ATTACHMENT_LOAD_OP_LOAD : VK_ATTACHMENT_LOAD_OP_CLEAR, VK_ATTACHMENT_STORE_OP_STORE,
            sl ? VK_ATTACHMENT_LOAD_OP_LOAD : p->has_stencil ? VK_ATTACHMENT_LOAD_OP_CLEAR : VK_ATTACHMENT_LOAD_OP_DONT_CARE,
            (sl || p->has_stencil) ? VK_ATTACHMENT_STORE_OP_STORE : VK_ATTACHMENT_STORE_OP_DONT_CARE,
            (dl || sl) ? VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL : VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL };
    }
    for (uint32_t i = 0; ms && i < ncol; i++) {
        if (!cf[i] || !(prm & (1u << i))) continue;
        rref[i].attachment = n; anyres = 1;
        atts[n++] = (VkAttachmentDescription2){ VK_STRUCTURE_TYPE_ATTACHMENT_DESCRIPTION_2, NULL, 0, (VkFormat)cf[i], VK_SAMPLE_COUNT_1_BIT,
            VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_STORE, VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_DONT_CARE,
            VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL };
    }
    VkResolveModeFlagBits dm = VK_RESOLVE_MODE_NONE, sm = VK_RESOLVE_MODE_NONE;
    const uint32_t df = p->dfmt ? p->dfmt : VK_FORMAT_D32_SFLOAT;
    if (ms && p->has_depth && (dmode || smode)) {
        dm = df == VK_FORMAT_S8_UINT ? VK_RESOLVE_MODE_NONE : nvmtl_resolve_mode(dmode, 0);
        sm = p->has_stencil ? nvmtl_resolve_mode(smode, 1) : VK_RESOLVE_MODE_NONE;
        if (dm && sm && dm != sm && !g_dsr_indep) dm = sm = VK_RESOLVE_MODE_SAMPLE_ZERO_BIT;
        if (p->has_stencil && df != VK_FORMAT_S8_UINT && (!dm || !sm) && !g_dsr_indep_none) { if (!dm) dm = VK_RESOLVE_MODE_SAMPLE_ZERO_BIT; if (!sm) sm = VK_RESOLVE_MODE_SAMPLE_ZERO_BIT; }
        drref.attachment = n;
        atts[n++] = (VkAttachmentDescription2){ VK_STRUCTURE_TYPE_ATTACHMENT_DESCRIPTION_2, NULL, 0, (VkFormat)df, VK_SAMPLE_COUNT_1_BIT,
            VK_ATTACHMENT_LOAD_OP_DONT_CARE, dm ? VK_ATTACHMENT_STORE_OP_STORE : VK_ATTACHMENT_STORE_OP_DONT_CARE,
            VK_ATTACHMENT_LOAD_OP_DONT_CARE, sm ? VK_ATTACHMENT_STORE_OP_STORE : VK_ATTACHMENT_STORE_OP_DONT_CARE,
            VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL };
    }
    VkSubpassDescriptionDepthStencilResolve dsr = { VK_STRUCTURE_TYPE_SUBPASS_DESCRIPTION_DEPTH_STENCIL_RESOLVE, NULL, dm, sm, &drref };
    VkSubpassDescription2 sp = { VK_STRUCTURE_TYPE_SUBPASS_DESCRIPTION_2, drref.attachment != VK_ATTACHMENT_UNUSED ? (const void *)&dsr : NULL, 0,
        VK_PIPELINE_BIND_POINT_GRAPHICS, 0, 0, NULL, ncol, ncol ? cref : NULL, anyres ? rref : NULL, p->has_depth ? &dref : NULL, 0, NULL };
    const VkSubpassDependency2 deps2[2] = {
        { VK_STRUCTURE_TYPE_SUBPASS_DEPENDENCY_2, NULL, VK_SUBPASS_EXTERNAL, 0, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
          VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT, 0, 0 },
        { VK_STRUCTURE_TYPE_SUBPASS_DEPENDENCY_2, NULL, 0, VK_SUBPASS_EXTERNAL, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
          VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT, 0, 0 } };
    VkRenderPassCreateInfo2 rci = { VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO_2, NULL, 0, n, atts, 1, &sp, 2, deps2, 0, NULL };
    return pvkCreateRenderPass2(g_dev, &rci, NULL, out);
}
typedef struct { uint64_t identity; uint32_t load, prm, dmode, smode; void *rp; } nvmtl_shape_rp;
static uint64_t g_shape_identity;
static nvmtl_shape_rp *g_srp; static uint32_t g_nsrp, g_csrp;
#define NVMTL_SRP_MAX 65536
static void *nvmtl_rp_shape_variant(nvk_pipeline *p, uint32_t load, uint32_t prm, uint32_t dmode, uint32_t smode)
{
    uint32_t cf[NVMTL_NCOL], rmask, ncol = nvmtl_rt_shape(p, cf, &rmask), keep = 0;
    for (uint32_t i = 0; i < ncol; i++) if (cf[i]) keep |= NVMTL_LOAD_COLOUR(i);
    if (p->has_depth) keep |= NVMTL_LOAD_DEPTH | (p->has_stencil ? NVMTL_LOAD_STENCIL : 0u);
    load &= keep;
    void *rp = NULL;
    pthread_mutex_lock(&g_rpv_lock);
    if (!p->shape_identity) {
        if (g_shape_identity == UINT64_MAX) { pthread_mutex_unlock(&g_rpv_lock); return NULL; }
        p->shape_identity = ++g_shape_identity;
    }
    for (uint32_t i = 0; i < g_nsrp; i++)
        if (g_srp[i].identity == p->shape_identity && g_srp[i].load == load && g_srp[i].prm == prm && g_srp[i].dmode == dmode && g_srp[i].smode == smode) { rp = g_srp[i].rp; break; }
    if (!rp && g_nsrp == g_csrp && g_csrp < NVMTL_SRP_MAX) {
        uint32_t cap = g_csrp ? g_csrp * 2u : 16u;
        nvmtl_shape_rp *gx = (nvmtl_shape_rp *)realloc(g_srp, cap * sizeof *gx);
        if (gx) { g_srp = gx; g_csrp = cap; }
    }
    if (!rp && g_nsrp < g_csrp) {
        VkRenderPass made = VK_NULL_HANDLE;
        VkResult r = nvmtl_make_render_pass_shape(p, load, prm, dmode, smode, &made);
        if (r == VK_SUCCESS) {
            g_srp[g_nsrp++] = (nvmtl_shape_rp){ p->shape_identity, load, prm, dmode, smode, (void *)made }; rp = (void *)made;
            nvlog("vk: par1 resolve-shape render pass made (%u colour slot(s), %u samples, resolves colour 0x%x of 0x%x, depth filter %d, stencil filter %d, load 0x%x)",
                  ncol, p->samples, prm, rmask, (int)dmode - 1, (int)smode - 1, load);
        } else nvlog("vk: par1 resolve-shape vkCreateRenderPass2 -> %d (%s)", r, pvkCreateRenderPass2 ? "the device refused it" : "vkCreateRenderPass2 not loaded");
    } else if (!rp) nvlog("vk: par1 resolve-shape list full (%u) — this pass is REFUSED", g_nsrp);
    pthread_mutex_unlock(&g_rpv_lock);
    return rp;
}
int nvmtl_vk_image_view_create_range_swz(nvk_image *img, uint32_t vkfmt, int a8, uint32_t baseLevel, uint32_t levelCount, uint32_t baseLayer,
                                         uint32_t layerCount, uint32_t mtl_view_type, const uint8_t mtl_swz[4], void **out_view)
{
    void *v = NULL;
    if (nvmtl_vk_image_view_create_range(img, vkfmt, a8, baseLevel, levelCount, baseLayer, layerCount, mtl_view_type, &v)) return -1;
    pvkDestroyImageView(g_dev, (VkImageView)v, NULL);
    static const VkComponentSwizzle a8map[6] = { VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ONE, VK_COMPONENT_SWIZZLE_ZERO,
                                                 VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_R };
    VkComponentSwizzle m[4];
    for (int i = 0; i < 4; i++) {
        uint8_t s = mtl_swz[i] <= 5 ? mtl_swz[i] : (uint8_t)(2 + i);
        m[i] = a8 ? a8map[s] : (VkComponentSwizzle)(s + 1);
    }
    const uint32_t mips = img->mips ? img->mips : 1, layers = img->layers ? img->layers : 1;
    if (!levelCount) levelCount = mips - baseLevel;
    if (!layerCount) layerCount = layers - baseLayer;
    VkImageViewType vt = mtl_view_type == 0 ? VK_IMAGE_VIEW_TYPE_1D : mtl_view_type == 1 ? VK_IMAGE_VIEW_TYPE_1D_ARRAY : (mtl_view_type == 3 || mtl_view_type == 8) ? VK_IMAGE_VIEW_TYPE_2D_ARRAY
                       : mtl_view_type == 5 ? VK_IMAGE_VIEW_TYPE_CUBE : mtl_view_type == 6 ? VK_IMAGE_VIEW_TYPE_CUBE_ARRAY : mtl_view_type == 7 ? VK_IMAGE_VIEW_TYPE_3D : VK_IMAGE_VIEW_TYPE_2D;
    if (mtl_view_type == 7) layerCount = 1;
    VkImageViewCreateInfo vci = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, (VkImage)img->img, vt, (VkFormat)vkfmt, { m[0], m[1], m[2], m[3] },
        { VK_IMAGE_ASPECT_COLOR_BIT, baseLevel, levelCount, baseLayer, layerCount } };
    VkImageViewUsageCreateInfo uci;
    int vg = nvmtl_vk_view_guard(img, vkfmt, a8, vci.viewType, vci.subresourceRange.levelCount, &uci, "image_view_create(swizzle)");
    if (vg < 0) return -1;
    if (vg > 0) {
        uci.usage &= ~(VkImageUsageFlags)VK_IMAGE_USAGE_STORAGE_BIT; vci.pNext = &uci; }
    else { uci = (VkImageViewUsageCreateInfo){ VK_STRUCTURE_TYPE_IMAGE_VIEW_USAGE_CREATE_INFO, NULL,
               (VkImageUsageFlags)(img->usage ? img->usage : (VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT)) & ~(VkImageUsageFlags)VK_IMAGE_USAGE_STORAGE_BIT };
           if (uci.usage) vci.pNext = &uci; }
    VkImageView vv = VK_NULL_HANDLE;
    VKCK(pvkCreateImageView(g_dev, &vci, NULL, &vv), "vkCreateImageView(swizzle)");
    *out_view = vv;
    return 0;
}
int nvmtl_vk_pass_kind(const nvk_pass *ps)
{
    int legacy = ps->col[0] && ps->colview[0] == ps->col[0]->view && !ps->res[0] && !ps->ds && !ps->dsview;
    int allLoad = 1, anyRes = 0, any = 0;
    for (uint32_t i = 0; i < NVMTL_NCOL; i++) {
        if (!ps->col[i]) continue;
        if (i) legacy = 0;
        any = 1;
        if (!(ps->load & NVMTL_LOAD_COLOUR(i))) allLoad = 0;
        if (ps->res[i]) anyRes = 1;
    }
    if (ps->ds) {
        any = 1;
        if (!(ps->load & NVMTL_LOAD_DEPTH) || (nvmtl_fmt_stencil(ps->ds->fmt) && !(ps->load & NVMTL_LOAD_STENCIL))) allLoad = 0;
    }
    if (ps->dres || ps->layers > 1) { legacy = 0; anyRes |= ps->dres != NULL; }
    if (legacy) return 0;
    if (!any || (allLoad && !anyRes)) return 1;
    return 2;
}
#define NVMTL_NSHAPE 32
static struct { nvk_rtsig k; uint32_t rmask; nvk_pipeline p; } g_shapes[NVMTL_NSHAPE]; static uint32_t g_nshapes;
int nvmtl_vk_cmd_empty_pass(nvk_cmdbuf *c, const nvk_pass *ps)
{
    nvk_rtsig k; memset(&k, 0, sizeof k); uint32_t rmask = 0, samples = 0;
    for (uint32_t i = 0; i < NVMTL_NCOL; i++) {
        if (!ps->col[i]) continue;
        uint32_t s = ps->col[i]->samples > 1 ? ps->col[i]->samples : 1u;
        if (samples && s != samples) { nvlog("encoder: apps2 a pass with no draws mixes %u- and %u-sample attachments — REFUSED", samples, s); return -1; }
        uint32_t vf = ps->colfmt[i] ? ps->colfmt[i] : ps->col[i]->fmt;
        samples = s; k.cfmts[i] = vf; k.ncol = i + 1;
        if (ps->res[i]) { uint32_t rf = ps->resfmt[i] ? ps->resfmt[i] : ps->res[i]->fmt;
            if (s > 1 && rf != vf) { nvlog("encoder: emptyfmt a pass with no draws resolves format %u into format %u — REFUSED (Vulkan and Metal both require one format)", vf, rf); return -1; }
            rmask |= 1u << i; }
    }
    if (ps->ds) {
        uint32_t s = ps->ds->samples > 1 ? ps->ds->samples : 1u;
        if (samples && s != samples) { nvlog("encoder: apps2 a pass with no draws mixes %u- and %u-sample attachments — REFUSED", samples, s); return -1; }
        samples = s; k.has_depth = 1; k.dfmt = ps->ds->fmt;
    }
    k.samples = samples ? samples : 1u;
    if (k.samples == 1) rmask = 0;
    pthread_mutex_lock(&g_rpv_lock);
    nvk_pipeline *p = NULL;
    for (uint32_t i = 0; i < g_nshapes; i++) if (!memcmp(&g_shapes[i].k, &k, sizeof k) && g_shapes[i].rmask == rmask) { p = &g_shapes[i].p; break; }
    if (!p && g_nshapes < NVMTL_NSHAPE) {
        nvk_pipeline *q = &g_shapes[g_nshapes].p; memset(q, 0, sizeof *q);
        q->ncol = k.ncol; memcpy(q->cfmts, k.cfmts, sizeof q->cfmts); q->cfmt = k.cfmts[0];
        q->has_depth = (int)k.has_depth; q->dfmt = k.dfmt; q->has_stencil = (uint32_t)nvmtl_fmt_stencil(k.dfmt); q->samples = k.samples; q->rmask = rmask;
        VkRenderPass made = VK_NULL_HANDLE; VkResult r = nvmtl_make_render_pass(q, 0, &made);
        if (r == VK_SUCCESS) { q->rp = (void *)made; g_shapes[g_nshapes].k = k; g_shapes[g_nshapes].rmask = rmask; g_nshapes++; p = q; }
        else nvlog("encoder: apps2 empty-pass vkCreateRenderPass -> %d", r);
    }
    pthread_mutex_unlock(&g_rpv_lock);
    if (!p) { static int said; if (!said++) nvlog("encoder: apps2 no empty pass for this shape (%u kept) — its clears and resolves are NOT applied", g_nshapes); return -1; }
    if (nvmtl_vk_cmd_begin_pass(c, ps, p)) return -1;
    nvmtl_vk_cmd_end_render(c);
    return 0;
}
int nvmtl_vk_cmd_begin_render(nvk_cmdbuf *c, nvk_image *img, nvk_pipeline *p, const float clear[4])
{ return nvmtl_vk_cmd_begin_render_ex(c, img, p, clear, 0); }
void nvmtl_vk_cmd_set_viewport(nvk_cmdbuf *c, float x, float y, float w, float h, float zn, float zf)
{
    /* Vulkan requires minDepth/maxDepth in [0,1] and NVK asserts it (nvk_emit_viewport "0.0 <= zmin && zmin <= 1.0");
       Apple GPUs take any value. 1.10 crash reports (10-11, RTX 4060): mediaanalysisd's Live Text draw aborted there on
       every run. The !(>=) form also sends NaN to Metal's defaults, 0 and 1. */
    if (!(zn >= 0.0f)) zn = 0.0f; else if (zn > 1.0f) zn = 1.0f;
    if (!(zf >= 0.0f)) zf = isnan(zf) ? 1.0f : 0.0f; else if (zf > 1.0f) zf = 1.0f;
    VkViewport vp = { x, y + h, w, -h, zn, zf }; pvkCmdSetViewport(c->cb, 0, 1, &vp);
}
static void nvmtl_vk_zero_dummies(void)
{
    static int done;
    if (done) return;
    done = 1;
    nvk_image *imgs[2] = { &g_dummyImg, g_dummyStex.view ? &g_dummyStex : NULL };
    VkCommandPool pool = VK_NULL_HANDLE; VkCommandBuffer cb = VK_NULL_HANDLE; VkFence fence = VK_NULL_HANDLE;
    VkCommandPoolCreateInfo cpci = { VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, NULL, 0, g_qfam };
    if (pvkCreateCommandPool(g_dev, &cpci, NULL, &pool) != VK_SUCCESS) { nvlog("vk: dummy zero-fill SKIPPED — vkCreateCommandPool failed; an unbound texture still samples uninitialised memory"); return; }
    VkCommandBufferAllocateInfo cbai = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, NULL, pool, VK_COMMAND_BUFFER_LEVEL_PRIMARY, 1 };
    if (pvkAllocateCommandBuffers(g_dev, &cbai, &cb) != VK_SUCCESS) { nvlog("vk: dummy zero-fill SKIPPED — vkAllocateCommandBuffers failed; an unbound texture still samples uninitialised memory"); return; }
    VkCommandBufferBeginInfo bi = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, NULL, VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT, NULL };
    if (pvkBeginCommandBuffer(cb, &bi) != VK_SUCCESS) { nvlog("vk: dummy zero-fill SKIPPED — vkBeginCommandBuffer failed; an unbound texture still samples uninitialised memory"); return; }
    VkImageSubresourceRange range = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 };
    VkClearColorValue zero; memset(&zero, 0, sizeof zero);
    for (int k = 0; k < 2; k++) {
        if (!imgs[k] || !imgs[k]->img) continue;
        VkImageLayout fin = (imgs[k] == &g_dummyStex) ? VK_IMAGE_LAYOUT_GENERAL : VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
        VkImageMemoryBarrier toDst = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL,
            0, VK_ACCESS_TRANSFER_WRITE_BIT, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)imgs[k]->img, range };
        pvkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &toDst);
        pvkCmdClearColorImage(cb, (VkImage)imgs[k]->img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &zero, 1, &range);
        VkImageMemoryBarrier toFin = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL,
            VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_SHADER_READ_BIT, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, fin,
            VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)imgs[k]->img, range };
        pvkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, 0, 0, NULL, 0, NULL, 1, &toFin);
        imgs[k]->layout = (uint32_t)fin;
    }
    if (pvkEndCommandBuffer(cb) != VK_SUCCESS) { nvlog("vk: dummy zero-fill SKIPPED — vkEndCommandBuffer failed; an unbound texture still samples uninitialised memory"); return; }
    VkFenceCreateInfo fci = { VK_STRUCTURE_TYPE_FENCE_CREATE_INFO, NULL, 0 };
    if (pvkCreateFence(g_dev, &fci, NULL, &fence) != VK_SUCCESS) { nvlog("vk: dummy zero-fill SKIPPED — vkCreateFence failed; an unbound texture still samples uninitialised memory"); return; }
    VkSubmitInfo si = { VK_STRUCTURE_TYPE_SUBMIT_INFO, NULL, 0, NULL, NULL, 1, &cb, 0, NULL };
    VkResult sr = pvkQueueSubmit(g_queue, 1, &si, fence);
    if (sr != VK_SUCCESS) { nvlog("vk: dummy zero-fill FAILED — vkQueueSubmit -> %d; an unbound texture still samples uninitialised memory", sr); pvkDestroyFence(g_dev, fence, NULL); return; }
    VkResult wr = pvkWaitForFences(g_dev, 1, &fence, VK_TRUE, 2000000000ull);
    pvkDestroyFence(g_dev, fence, NULL);
    if (wr != VK_SUCCESS) { nvlog("vk: dummy zero-fill did not complete — vkWaitForFences -> %d; an unbound texture may still sample uninitialised memory", wr); return; }
    nvlog("vk: dummy images ZEROED and transitioned (sampled -> SHADER_READ_ONLY_OPTIMAL%s) — a declared-but-unbound texture now reads opaque black, not uninitialised VRAM",
          g_dummyStex.view ? ", storage -> GENERAL" : "; no storage dummy");
}

int nvmtl_vk_cmd_clear_image(nvk_cmdbuf *c, nvk_image *img, const float clear[4])
{
    VkImageSubresourceRange range = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 };
    VkImageMemoryBarrier toDst = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL,
        0, VK_ACCESS_TRANSFER_WRITE_BIT, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)img->img, range };
    pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT,
                          0, 0, NULL, 0, NULL, 1, &toDst);
    VkClearColorValue cv; nvmtl_clear_colour(&cv, img->fmt, clear);
    pvkCmdClearColorImage(c->cb, (VkImage)img->img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &cv, 1, &range);
    VkImageMemoryBarrier toSrc = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL,
        VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_TRANSFER_READ_BIT,
        VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
        VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)img->img, range };
    pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT,
                          0, 0, NULL, 0, NULL, 1, &toSrc);
    img->layout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
    return 0;
}

void nvmtl_vk_cmd_draw(nvk_cmdbuf *c, uint32_t first, uint32_t count, uint32_t instances, uint32_t baseInstance)
{ if (c->nobind) return; pvkCmdDraw(c->cb, count, instances ? instances : 1, first, baseInstance); }
void nvmtl_vk_cmd_set_scissor(nvk_cmdbuf *c, int32_t x, int32_t y, uint32_t w, uint32_t h)
{ VkRect2D r = { { x, y }, { w, h } }; pvkCmdSetScissor(c->cb, 0, 1, &r); }
void nvmtl_vk_cmd_bind_pipeline(nvk_cmdbuf *c, nvk_pipeline *p) { pvkCmdBindPipeline(c->cb, VK_PIPELINE_BIND_POINT_GRAPHICS, (VkPipeline)p->pipe); }
int nvmtl_vk_cmd_set_topology(nvk_cmdbuf *c, unsigned mtlType) {
    static const VkPrimitiveTopology map[5] = { VK_PRIMITIVE_TOPOLOGY_POINT_LIST, VK_PRIMITIVE_TOPOLOGY_LINE_LIST,
        VK_PRIMITIVE_TOPOLOGY_LINE_STRIP, VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST, VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP };
    if (c->nobind) return 0;
    if (mtlType > 4) { static int said; if (said++ < 4) nvlog("G14 draw: unknown MTLPrimitiveType %u — NOT drawn", mtlType); return -1; }
    if (mtlType < 3 && !g_topo_any) { static int said2; if (said2++ < 4) nvlog("G14 draw: point/line primitives need "
        "dynamicPrimitiveTopologyUnrestricted, which this device does not report — NOT drawn (was: rasterised as triangles)"); return -1; }
    pvkCmdSetPrimitiveTopology(c->cb, map[mtlType]);
    pvkCmdSetPrimitiveRestartEnable(c->cb, (mtlType == 2 || mtlType == 4) ? VK_TRUE : VK_FALSE);
    if (mtlType != 3) { static int told[5]; if (!told[mtlType]++) nvlog("G14 first draw of MTLPrimitiveType %u in this process", mtlType); }
    return 0;
}
int nvmtl_vk_cmd_vertex_inputs(nvk_cmdbuf *c, const nvk_pipeline *p, const uint32_t *vstride, uint32_t vset)
{
    if (c->nobind || !p || !p->vin_mask) return 0;
    nvmtl_bindtable *t = (nvmtl_bindtable *)c->table;
    for (uint32_t i = 0; i < NVMTL_NVIN; i++) if (p->vin_mask & (1u << i)) {
        nvmtl_slot *sl = t ? &t->s[0][i] : NULL;
        if (!sl || sl->kind != NVMTL_SLOT_BUF || !sl->h) { static int said; if (said++ < 8) nvlog("G16 draw: the pipeline fetches vertex "
            "attributes from buffer index %u and nothing is bound there — NOT drawn", i); return -1; }
        VkBuffer b = (VkBuffer)sl->h; VkDeviceSize off = sl->off;
        if (!p->vin_dyn) { pvkCmdBindVertexBuffers(c->cb, i, 1, &b, &off); continue; }
        VkDeviceSize st = p->vin_stride[i];
        if (p->vin_dyn & (1u << i)) {
            if (!vstride || !(vset & (1u << i))) { static int said; if (said++ < 8) nvlog("apps-dyn draw: buffer index %u has a dynamic stride "
                "(MTLBufferLayoutStrideDynamic) and no attributeStride was set there — NOT drawn", i); return -1; }
            st = vstride[i];
            if (st > g_lim.maxVertexInputBindingStride) { static int said; if (said++ < 8) nvlog("apps-dyn draw: attributeStride %llu at "
                "buffer index %u is past maxVertexInputBindingStride %u — NOT drawn", (unsigned long long)st, i, g_lim.maxVertexInputBindingStride); return -1; }
        }
        pvkCmdBindVertexBuffers2(c->cb, i, 1, &b, &off, NULL, &st);
    }
    return 0;
}
void nvmtl_vk_cmd_set_cull(nvk_cmdbuf *c, int mode)
{
    int m = mode & 3;
    pvkCmdSetCullMode(c->cb, m == 1 ? VK_CULL_MODE_FRONT_BIT : m == 2 ? VK_CULL_MODE_BACK_BIT : VK_CULL_MODE_NONE);
    pvkCmdSetFrontFace(c->cb, (mode & 4) ? VK_FRONT_FACE_COUNTER_CLOCKWISE : VK_FRONT_FACE_CLOCKWISE);
}
void nvmtl_vk_cmd_draw_indexed(nvk_cmdbuf *c, nvk_buffer *idx, size_t offset, uint32_t count, int is32,
                               uint32_t instances, int32_t baseVertex, uint32_t baseInstance)
{
    pvkCmdBindIndexBuffer(c->cb, (VkBuffer)idx->buf, offset, is32 ? VK_INDEX_TYPE_UINT32 : VK_INDEX_TYPE_UINT16);
    if (c->nobind) return;
    pvkCmdDrawIndexed(c->cb, count, instances ? instances : 1, 0, baseVertex, baseInstance);
}

void nvmtl_vk_cmd_set_blend_color(nvk_cmdbuf *c, const float rgba[4]) { pvkCmdSetBlendConstants(c->cb, rgba); }
void nvmtl_vk_cmd_set_stencil_ref(nvk_cmdbuf *c, uint32_t front, uint32_t back)
{
    pvkCmdSetStencilReference(c->cb, VK_STENCIL_FACE_FRONT_BIT, front);
    pvkCmdSetStencilReference(c->cb, VK_STENCIL_FACE_BACK_BIT, back);
}
void nvmtl_vk_cmd_set_stencil(nvk_cmdbuf *c, int enable, const uint32_t *f, const uint32_t *b)
{
    const uint32_t *o[2] = { f, b }; const VkStencilFaceFlags face[2] = { VK_STENCIL_FACE_FRONT_BIT, VK_STENCIL_FACE_BACK_BIT };
    pvkCmdSetStencilTestEnable(c->cb, enable ? VK_TRUE : VK_FALSE);
    for (int i = 0; i < 2; i++) {
        pvkCmdSetStencilOp(c->cb, face[i], (VkStencilOp)o[i][1], (VkStencilOp)o[i][3], (VkStencilOp)o[i][2], (VkCompareOp)o[i][0]);
        pvkCmdSetStencilCompareMask(c->cb, face[i], o[i][4]);
        pvkCmdSetStencilWriteMask(c->cb, face[i], o[i][5]);
    }
}
void nvmtl_vk_cmd_set_depth_bias(nvk_cmdbuf *c, float constant, float slope, float clamp)
{ pvkCmdSetDepthBias(c->cb, constant, clamp, slope); }
void nvmtl_vk_cmd_set_line_width(nvk_cmdbuf *c, float w)
{
    if (!g_wide) return;
    float px = !(w >= 1.0f) ? 1.0f : w > 64.0f ? 64.0f : (float)(int)(w + 0.5f);
    pvkCmdSetLineWidth(c->cb, px);
}

int nvmtl_vk_fill_buffer(nvk_queue *q, nvk_buffer *b, size_t offset, size_t size, uint8_t value)
{
    nvk_cmdbuf c; if (nvmtl_vk_cmd_begin(q, &c)) return -1;
    if (nvmtl_vk_cmd_fill_buffer(&c, b, offset, size, value)) { nvmtl_vk_cmd_abandon(&c); return -1; }
    return nvmtl_vk_submit_wait(&c);
}
static int nvmtl_counter_flush(nvk_cmdbuf *c);
static void nvmtl_counter_retire(nvk_cmdbuf *c);
static int nvmtl_counter_reset_finish(nvk_cmdbuf *c, VkCommandBuffer *reset);
static int g_pt_left; static uint64_t g_pt_seq;
void nvmtl_vk_pt_note(nvk_cmdbuf *c, uint32_t fs)
{
    if (!c || !c->pt_open || !c->pt_pool || c->pt_n >= 128 || !fs) return;
    for (unsigned k = 0; k < 4; k++) { if (c->pt_fs[c->pt_n][k] == fs) return; if (!c->pt_fs[c->pt_n][k]) { c->pt_fs[c->pt_n][k] = fs; return; } }
}
int nvmtl_pt_armed(void)
{
    static uint64_t next; uint64_t now = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    if (g_pt_left <= 0 && now >= next) { next = now + 250000000ull;
        if (access("/tmp/nvmtl-passtime-request", F_OK) == 0) { unlink("/tmp/nvmtl-passtime-request"); g_pt_left = 400; nvlog("passtime: ARMED for 400 passes"); } }
    return g_pt_left > 0;
}
void nvmtl_vk_cmd_end_render(nvk_cmdbuf *c) {
    pvkCmdEndRenderPass(c->cb); c->in_rp = 0;
    if (c->pt_open && c->pt_pool) { pvkCmdWriteTimestamp(c->cb, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, (VkQueryPool)c->pt_pool, 2 * c->pt_n + 1); c->pt_n++; c->pt_open = 0; g_pt_left--; }
    (void)nvmtl_counter_flush(c); }

static void nvmtl_cmd_retire(nvk_cmdbuf *c)
{
    nvmtl_counter_retire(c);
    nvmtl_vk_cmd_retire_occlusion(c);
    if (c->timing_pool) {
        nvmtl_tspool_push((VkQueryPool)c->timing_pool);
        c->timing_pool = NULL;
    }
    for (unsigned i = 0; i < c->nfb; i++) pvkDestroyFramebuffer(g_dev, (VkFramebuffer)c->fbs[i], NULL);
    free(c->fbs); c->fbs = NULL; c->nfb = c->capfb = 0; c->fb = NULL;
    for (unsigned i = 0; i < c->nrv; i++) pvkDestroyImageView(g_dev, (VkImageView)c->rvs[i], NULL);
    free(c->rvs); c->rvs = NULL; c->nrv = c->caprv = 0;
    if (c->ndcache) {
        pthread_mutex_lock(&g_desc_lock);
        VkDescriptorSet back[NVMTL_DESC_BATCH_MAX];
        for (unsigned i = 0; i < c->ndcache && i < NVMTL_DESC_BATCH_MAX; i++) back[i] = (VkDescriptorSet)c->dcache[i];
        pvkFreeDescriptorSets(g_dev, (VkDescriptorPool)c->dcachepool, c->ndcache, back);
        pthread_mutex_unlock(&g_desc_lock);
        c->ndcache = 0; c->dcachepool = NULL;
    }
    if (c->nsets) {
        pthread_mutex_lock(&g_desc_lock);
        for (unsigned i = 0; i < c->nsets;) {
            VkDescriptorPool p = (VkDescriptorPool)c->sets[i].pool; VkDescriptorSet run[256]; unsigned n = 0;
            while (i < c->nsets && c->sets[i].pool == (void *)p && n < 256) run[n++] = (VkDescriptorSet)c->sets[i++].set;
            pvkFreeDescriptorSets(g_dev, p, n, run);
        }
        pthread_mutex_unlock(&g_desc_lock);
    }
    free(c->sets); c->sets = NULL; c->nsets = c->capsets = 0; c->nobind = 0; c->skipped = 0;
    free(c->table); c->table = NULL;
    nvmtl_address_page *page = c->address_pages;
    const int keep = nvmtl_keep_verdict(c->wseq);
    while (page) { nvmtl_address_page *next = page->next;
        if (keep >= 0 && !(keep > 0 && nvmtl_keep_page_push(page))) { nvmtl_vk_buffer_destroy(&page->buffer); free(page); }
        page = next; }
    c->address_pages = NULL; c->address_count = 0;
    if (c->cb && c->pool) {
        VkCommandBuffer cb = (VkCommandBuffer)c->cb;
        pvkFreeCommandBuffers(g_dev, (VkCommandPool)c->pool, 1, &cb);
        if (keep >= 0 && !(keep > 0 && nvmtl_keep_pool_push((VkCommandPool)c->pool))) pvkDestroyCommandPool(g_dev, (VkCommandPool)c->pool, NULL);
        c->cb = NULL; c->pool = NULL;
    }
}

int nvmtl_vk_cmd_abandon(nvk_cmdbuf *c)
{
    if (g_state != 1 || !c || !c->cb) return 0;
    g_cmd_abandoned++;
    nvmtl_cmd_retire(c);
    return 0;
}

static pthread_mutex_t nvmtl_submit_mutex = PTHREAD_RECURSIVE_MUTEX_INITIALIZER;
void (*nvmtl_pre_submit_hook)(void);
void (*nvmtl_pre_release_hook)(void *);
typedef struct { nvk_cmdbuf cb; unsigned begun, n, fills, copies, nhold; unsigned long long bytes; void *hold[4]; } nvmtl_prologue;
static __thread nvmtl_prologue *g_pre_cur;
static _Atomic unsigned long long g_pre_subs, g_pre_copies, g_pre_fills, g_pre_bytes, g_pre_fail;
static int nvmtl_prologue_on(void)
{
    static int on = -1;
    if (on < 0) {
        const char *e = getenv("NVMTL_PRESUBMIT_PROLOGUE"); on = !(e && e[0] == '0');
        nvlog("vk: pre-submit prologue %s (NVMTL_PRESUBMIT_PROLOGUE=%s)", on ? "ON - managed uploads and zero fills ride the submit they serve"
              : "OFF - every managed upload is its own submit, as before b78", e ? e : "unset");
    }
    return on;
}
int nvmtl_vk_pre_open(void)
{
    nvmtl_prologue *p = g_pre_cur;
    if (!p || p->nhold >= sizeof p->hold / sizeof p->hold[0]) return 0;
    if (!p->begun) {
        if (nvmtl_vk_cmd_begin(NULL, &p->cb)) { g_pre_fail++; nvlog("vk: pre-submit prologue begin FAILED - this flush submits on its own"); return 0; }
        p->begun = 1;
        VkMemoryBarrier mb = { VK_STRUCTURE_TYPE_MEMORY_BARRIER, NULL, VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_TRANSFER_READ_BIT | VK_ACCESS_TRANSFER_WRITE_BIT };
        pvkCmdPipelineBarrier(p->cb.cb, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 1, &mb, 0, NULL, 0, NULL);
    }
    return 1;
}
int nvmtl_vk_pre_copy(nvk_buffer *src, size_t srcOff, nvk_buffer *dst, size_t dstOff, size_t size)
{
    nvmtl_prologue *p = g_pre_cur;
    if (!p || !p->begun || !src || !dst) return -1;
    if (!size) return 0;
    if (srcOff > src->size || size > src->size - srcOff || dstOff > dst->size || size > dst->size - dstOff) return -1;
    if (p->fills && !p->copies) {
        VkMemoryBarrier mb = { VK_STRUCTURE_TYPE_MEMORY_BARRIER, NULL, VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_TRANSFER_READ_BIT | VK_ACCESS_TRANSFER_WRITE_BIT };
        pvkCmdPipelineBarrier(p->cb.cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 1, &mb, 0, NULL, 0, NULL);
    }
    VkBufferCopy bc = { srcOff, dstOff, size };
    pvkCmdCopyBuffer(p->cb.cb, (VkBuffer)src->buf, (VkBuffer)dst->buf, 1, &bc);
    p->n++; p->copies++; p->bytes += size;
    return 0;
}
int nvmtl_vk_pre_fill(nvk_buffer *b, size_t offset, size_t size, uint32_t word)
{
    nvmtl_prologue *p = g_pre_cur;
    if (!p || !p->begun || !b) return -1;
    if (!size) return 0;
    if ((offset | size) & 3 || offset > b->size || size > b->size - offset) return -1;
    pvkCmdFillBuffer(p->cb.cb, (VkBuffer)b->buf, offset, size, word);
    p->n++; p->fills++; p->bytes += size;
    return 0;
}
void nvmtl_vk_pre_hold(void *retained)
{
    nvmtl_prologue *p = g_pre_cur;
    if (p && retained && p->nhold < sizeof p->hold / sizeof p->hold[0]) p->hold[p->nhold++] = retained;
}
static void nvmtl_pre_drop(nvmtl_prologue *p)
{
    for (unsigned i = 0; i < p->nhold; i++) if (p->hold[i] && nvmtl_pre_release_hook) nvmtl_pre_release_hook(p->hold[i]);
    p->nhold = 0;
}
struct nvmtl_inflight { nvk_cmdbuf *c; VkFence fence; nvmtl_prologue pre; uint64_t pt[8]; };
nvmtl_inflight *nvmtl_vk_inflight_new(void) { return calloc(1, sizeof(nvmtl_inflight)); }
void nvmtl_vk_inflight_free(nvmtl_inflight *f) { free(f); }
void nvmtl_vk_gsub_nvk_stats(uint64_t *out, unsigned n)
{
    static void (*fn)(uint64_t *, unsigned); static int looked;
    if (!looked) { fn = g_lib ? (void (*)(uint64_t *, unsigned))dlsym(g_lib, "nvkmd_nvrm_gsub_stats") : NULL; looked = 1; }
    for (unsigned i = 0; i < n; i++) out[i] = 0;
    if (fn) fn(out, n); else if (n) out[0] = ~0ull;
}
static struct { const char *who; uint64_t n; uint64_t ns; } g_subwho[24];
static unsigned g_subwho_n;
static void nvmtl_subwho_note(const char *who, uint64_t ns)
{
    if (!who) who = "?";
    unsigned i = 0;
    for (; i < g_subwho_n; i++) if (g_subwho[i].who == who) break;
    if (i == g_subwho_n) {
        if (g_subwho_n >= 24) return;
        g_subwho[g_subwho_n].who = who; g_subwho[g_subwho_n].n = 0; g_subwho[g_subwho_n].ns = 0;
        i = g_subwho_n++;
    }
    g_subwho[i].n++; g_subwho[i].ns += ns;
    uint64_t total = 0;
    for (unsigned k = 0; k < g_subwho_n; k++) total += g_subwho[k].n;
    if ((total & 0xfff) == 0) {
        char line[1200]; size_t off = 0;
        for (unsigned k = 0; k < g_subwho_n && off < sizeof line - 90; k++) {
            int w = snprintf(line + off, sizeof line - off, "%s%s=%llu", off ? ", " : "",
                             g_subwho[k].who, (unsigned long long)g_subwho[k].n);
            if (w > 0) off += (size_t)w;
        }
        nvlog("vk: submit census over %llu submissions - %s", (unsigned long long)total, line);
    }
}
int nvmtl_vk_submit_wait_at(nvk_cmdbuf *c, const char *who)
{
    nvmtl_inflight f;
    const int r = nvmtl_vk_submit_begin_at(c, &f, who);
    return r ? r : nvmtl_vk_submit_finish(&f);
}
int nvmtl_vk_submit_begin_at(nvk_cmdbuf *c, nvmtl_inflight *f, const char *who)
{
    nvmtl_subwho_note(who, 0);
    memset(f, 0, sizeof *f); f->c = c;
    uint64_t *const pt = f->pt;
    if (g_state != 1 || !c || !c->cb) return -2;
    if (c->counter_error || nvmtl_counter_flush(c)) { nvmtl_cmd_retire(c); return -1; }
    VkCommandBuffer counter_reset = VK_NULL_HANDLE;
    if (nvmtl_counter_reset_finish(c, &counter_reset)) { nvmtl_cmd_retire(c); return -1; }
    c->timing.clock = nvmtl_clock_snapshot();
    c->timing.host_before_ns = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
    if (c->timing_pool && !c->in_rp)
        pvkCmdWriteTimestamp(c->cb, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, (VkQueryPool)c->timing_pool, 1);
    else c->timing.clock.valid = 0;
    pt[0] = nvmtl_perf_now();
    const int pro = nvmtl_prologue_on();
    if (!pro && nvmtl_pre_submit_hook) { const int k0 = g_perf_kind; g_perf_kind |= 1; nvmtl_pre_submit_hook(); g_perf_kind = k0; }
    const uint64_t sA = nvmtl_perf_now();
    const uint32_t wseq = nvmtl_keep_on() ? nvmtl_wit_record(c) : 0;
    VkResult result = pvkEndCommandBuffer(c->cb);
    const uint64_t sB = nvmtl_perf_now();
    if (result != VK_SUCCESS) { nvmtl_cmd_retire(c); return -1; }
    VkFenceCreateInfo fci = { VK_STRUCTURE_TYPE_FENCE_CREATE_INFO }; VkFence fence;
    result = pvkCreateFence(g_dev, &fci, NULL, &fence);
    const uint64_t sC = nvmtl_perf_now();
    if (result != VK_SUCCESS) { nvmtl_cmd_retire(c); return -1; }
    nvmtl_prologue *const pre = &f->pre;
    pthread_mutex_lock(&nvmtl_submit_mutex);
    if (pro && nvmtl_pre_submit_hook) {
        nvmtl_prologue *const up = g_pre_cur; g_pre_cur = pre;
        const int k0 = g_perf_kind; g_perf_kind |= 1; nvmtl_pre_submit_hook(); g_perf_kind = k0;
        g_pre_cur = up;
    }
    const uint64_t sD = nvmtl_perf_now();
    VkCommandBuffer cbs[3]; uint32_t ncb = 0;
    if (counter_reset) cbs[ncb++] = counter_reset;
    if (pre->begun) {
        VkResult pr = VK_SUCCESS;
        if (pre->n) {
            VkMemoryBarrier mb = { VK_STRUCTURE_TYPE_MEMORY_BARRIER, NULL, VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT };
            pvkCmdPipelineBarrier(pre->cb.cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, 0, 1, &mb, 0, NULL, 0, NULL);
            pr = pvkEndCommandBuffer(pre->cb.cb);
        }
        if (pre->n && pr == VK_SUCCESS) cbs[ncb++] = pre->cb.cb;
        else {
            if (pre->n) { g_pre_fail++; nvlog("vk: pre-submit prologue End -> %d - %u uploads/fills (%llu bytes) LOST, the GPU keeps the old bytes", pr, pre->n, pre->bytes); }
            nvmtl_cmd_retire(&pre->cb); nvmtl_pre_drop(pre); pre->begun = 0;
        }
    }
    cbs[ncb++] = c->cb;
    VkSubmitInfo si = { VK_STRUCTURE_TYPE_SUBMIT_INFO, NULL, 0, NULL, NULL, ncb, cbs, 0, NULL };
    c->wseq = wseq;
    nvmtl_wit_control(wseq);
    const uint64_t sE = nvmtl_perf_now();
    if (pt[0]) { pt[1] = pt[0] + (sA - pt[0]) + (sD - sC); pt[2] = pt[1] + (sB - sA) + (sE - sD); pt[3] = pt[2] + (sC - sB); }
    result = g_state == 1 ? pvkQueueSubmit(g_queue, 1, &si, fence) : VK_ERROR_DEVICE_LOST;
    pthread_mutex_unlock(&nvmtl_submit_mutex);
    pt[4] = nvmtl_perf_now();
    g_cmd_submitted++;
    if (result != VK_SUCCESS) {
        g_state = -1;
        nvlog("vk: queue submit -> %d; disabling backend and retaining uncertain in-flight resources", result);
        return -2;
    }
    f->fence = fence;
    return 0;
}
int nvmtl_vk_submit_finish(nvmtl_inflight *f)
{
    nvk_cmdbuf *const c = f->c; const VkFence fence = f->fence; uint64_t *const pt = f->pt;
    nvmtl_prologue *const pre = &f->pre;
    const long budget_ms = nvmtl_gpu_wait_budget_ms();
    const uint64_t w0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    VkResult wr; long waited_ms = 0;
    for (;;) {
        wr = pvkWaitForFences(g_dev, 1, &fence, VK_TRUE, 250ull * 1000 * 1000);
        waited_ms = (long)((clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - w0) / 1000000ull);
        if (wr != VK_TIMEOUT) break;
        if (waited_ms >= budget_ms) break;
    }
    pt[5] = nvmtl_perf_now();
    atomic_fetch_add(&g_submit_wait_ns, clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - w0);
    atomic_fetch_add(&g_submit_waits, 1);
    if (waited_ms > g_slow_submit_ms) {
        g_slow_submit_ms = waited_ms;
        if (waited_ms >= 1000)
            nvlog("vk: slowest submit so far %ld ms (budget %ld ms) - this is the wait the old 5 s watchdog called a dead device", waited_ms, budget_ms);
    }
    if (wr != VK_SUCCESS || g_state != 1) {
        g_state = -1;
        nvlog("vk: fence wait -> %d after %ld ms (budget %ld ms); disabling backend without retiring in-flight resources", wr, waited_ms, budget_ms);
        return -2;
    }
    if (c->wseq && c->wseq != NVMTL_WSEQ_BLIND) {
        if (nvmtl_keep_verdict(c->wseq) < 0) {
            g_state = -1;
            nvlog("vk: completion witness missing; disabling backend and retaining all in-flight resources");
            return -2;
        }
        c->wseq = 0;
    }
    c->timing.host_after_ns = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
    if (c->timing_pool && c->timing.clock.valid) {
        uint64_t pair[4] = {0};
        VkResult qr = pvkGetQueryPoolResults(g_dev, (VkQueryPool)c->timing_pool, 0, 2,
            sizeof pair, pair, 16, VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WITH_AVAILABILITY_BIT);
        if (qr == VK_SUCCESS && pair[1] && pair[3]) {
            c->timing.start_tick = pair[0]; c->timing.end_tick = pair[2]; c->timing.valid = 1;
            double a, b;
            if (nvmtl_vk_interval_seconds(&c->timing, &a, &b)) c->timing.valid = 0;
        }
    }
    if (c->pt_pool && c->pt_n) {
        uint64_t r[256] = {0}; const unsigned n = c->pt_n;
        if (pvkGetQueryPoolResults(g_dev, (VkQueryPool)c->pt_pool, 0, 2 * n, sizeof(uint64_t) * 2 * n, r, 8, VK_QUERY_RESULT_64_BIT) == VK_SUCCESS) {
            static FILE *pf; if (!pf) { char fp[512]; const char *td = getenv("TMPDIR"); snprintf(fp, sizeof fp, "%s/nvmtl-passtime-%d.log", td && *td ? td : "/tmp", (int)getpid()); pf = fopen(fp, "a"); }
            const double us = g_lim.timestampPeriod / 1000.0; const uint64_t mask = g_timestamp_bits >= 64 ? ~0ull : ((1ull << g_timestamp_bits) - 1);
            const uint64_t seq = ++g_pt_seq;
            for (unsigned i = 0; pf && i < n; i++) {
                const double gpu = (double)((r[2 * i + 1] - r[2 * i]) & mask) * us;
                const double gap = i ? (double)((r[2 * i] - r[2 * i - 1]) & mask) * us : 0.0;
                fprintf(pf, "cb %llu pass %u %ux%u fmt %u gpu_us %.1f gap_us %.1f fs %08x %08x %08x %08x\n", (unsigned long long)seq, i, c->pt_w[i], c->pt_h[i], c->pt_f[i], gpu, gap,
                        c->pt_fs[i][0], c->pt_fs[i][1], c->pt_fs[i][2], c->pt_fs[i][3]);
            }
            if (pf) fflush(pf);
        }
        pvkDestroyQueryPool(g_dev, (VkQueryPool)c->pt_pool, NULL); c->pt_pool = NULL; c->pt_n = 0;
    }
    pvkDestroyFence(g_dev, fence, NULL);
    if (c->nsets > g_peak_sets) { g_peak_sets = c->nsets; nvlog("vk: commit high-water: %u descriptor sets = %u draws in one command buffer (%u pools open)", c->nsets, c->nsets / 2, g_ndescPools); }
    if (c->nfb > g_peak_fbs) g_peak_fbs = c->nfb;
    if (c->skipped) nvlog("vk: commit with %u draws/dispatches SKIPPED (no descriptor set could be allocated)", c->skipped);
    const unsigned perf_sets = c->nsets;
    pt[6] = nvmtl_perf_now();
    if (pre->begun) {
        pre->cb.wseq = c->wseq; nvmtl_cmd_retire(&pre->cb); nvmtl_pre_drop(pre);
        const unsigned long long k = g_pre_subs++; g_pre_copies += pre->copies; g_pre_fills += pre->fills; g_pre_bytes += pre->bytes;
        if (!k || ((k + 1) & 0xFFF) == 0)
            nvlog("vk: pre-submit prologue census: %llu submits carried %llu managed uploads + %llu zero fills (%llu KB) - each WAS its own submit; %llu prologues failed",
                  k + 1, (unsigned long long)g_pre_copies, (unsigned long long)g_pre_fills, (unsigned long long)(g_pre_bytes >> 10), (unsigned long long)g_pre_fail);
    }
    nvmtl_cmd_retire(c);
    pt[7] = nvmtl_perf_now();
    nvmtl_perf_submit(perf_sets, pt);
    return 0;
}

static void nvmtl_vk_buffer_destroy_body(nvk_buffer *b);
void nvmtl_vk_buffer_destroy(nvk_buffer *b)
{
    const uint64_t t0 = nvmtl_perf_now();
    const size_t n = b ? b->size : 0;
    nvmtl_vk_buffer_destroy_body(b);
    nvmtl_perf_note(NVP_BUFDEL, t0, n);
}
static void nvmtl_vk_buffer_destroy_body(nvk_buffer *b)
{
    if (g_state != 1 || !b || !b->buf) return;
    g_buf_gone++;
    pvkDestroyBuffer(g_dev, (VkBuffer)b->buf, NULL);
    if (b->pool) pool_buffer_release(b);
    if (!b->placed && !b->imported && b->mem && b->map && pvkUnmapMemory) { pvkUnmapMemory(g_dev, (VkDeviceMemory)b->mem); b->map = NULL; }
    if (b->bar1) g_bar1_bytes -= b->alloc;
    if (!b->placed && b->mem) { pvkFreeMemory(g_dev, (VkDeviceMemory)b->mem, NULL); g_alloc_bytes -= b->alloc; }
    memset(b, 0, sizeof *b);
}

void nvmtl_vk_buffer_retire(nvk_buffer *b)
{
    if (g_state != 1 || !b || !b->buf) return;
    if (!bpark_on() || b->pool || b->placed || b->imported || b->moff || !b->mem || !b->alloc || b->alloc > NVMTL_BUFPARK_MAX1) {
        nvmtl_vk_buffer_destroy(b); return; }
    const uint64_t t0 = nvmtl_perf_now();
    const size_t n = b->size;
    nvk_buffer ev[NVMTL_BUFPARK_N + 1]; unsigned nev = 0;
    pthread_mutex_lock(&g_bpark_lock);
    while (g_bpark_n && (g_bpark_n >= NVMTL_BUFPARK_N || g_bpark_bytes + b->alloc > NVMTL_BUFPARK_BYTES)) {
        ev[nev++] = g_bpark[0]; g_bpark_bytes -= g_bpark[0].alloc;
        memmove(&g_bpark[0], &g_bpark[1], (g_bpark_n - 1) * sizeof g_bpark[0]); g_bpark_n--; }
    g_bpark[g_bpark_n++] = *b; g_bpark_bytes += b->alloc;
    pthread_mutex_unlock(&g_bpark_lock);
    g_buf_gone++;
    memset(b, 0, sizeof *b);
    for (unsigned k = 0; k < nev; k++) bpark_free(&ev[k]);
    if (nev) atomic_fetch_add(&g_bpark_evicted, nev);
    atomic_fetch_add(&g_bpark_parked, 1); bpark_census("park");
    nvmtl_perf_note(NVP_BUFDEL, t0, n);
}

void nvmtl_vk_image_destroy(nvk_image *i)
{
    if (g_state != 1 || !i || !i->img) return;
    g_img_gone++;
    if (i->view) pvkDestroyImageView(g_dev, (VkImageView)i->view, NULL);
    pvkDestroyImage(g_dev, (VkImage)i->img, NULL);
    if (!i->placed && i->mem) { pvkFreeMemory(g_dev, (VkDeviceMemory)i->mem, NULL); g_alloc_bytes -= i->alloc; if (i->sysmem) { g_sys_bytes -= i->alloc; g_sys_align -= (i->alloc + 65535ull) & ~65535ull; } else { g_vram_bytes -= i->alloc; g_img_align -= (i->alloc + 65535ull) & ~65535ull; }    }
    memset(i, 0, sizeof *i);
}

static uint64_t g_res1_off, g_res1_on, g_res1_refused, g_res1_off_b, g_res1_on_b;
void nvmtl_vk_res1_stats(uint64_t out[6]) { out[0] = g_res1_off; out[1] = g_res1_on; out[2] = g_res1_refused;
    out[3] = g_res1_off_b >> 20; out[4] = g_res1_on_b >> 20; out[5] = g_res1_calls; }
int nvmtl_vk_image_reback(nvk_image *i, int to_sysmem, int a8)
{
    if (g_state != 1 || !i || !i->img || i->placed || !i->usage || i->vpair || i->samples > 1 || i->mtl_type == 4 || i->mtl_type > 7
        || (i->usage & VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT)) return -1;
    if (!to_sysmem == !i->sysmem) return 1;
    const uint32_t t = i->mtl_type;
    VkImageCreateInfo ici = { VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, NULL, (VkImageCreateFlags)i->vkflags,
        t == 7 ? VK_IMAGE_TYPE_3D : t < 2 ? VK_IMAGE_TYPE_1D : VK_IMAGE_TYPE_2D, (VkFormat)i->fmt,
        { i->w, i->h, t == 7 ? i->layers : 1 }, i->mips ? i->mips : 1, t == 7 ? 1 : i->layers, VK_SAMPLE_COUNT_1_BIT,
        VK_IMAGE_TILING_OPTIMAL, (VkImageUsageFlags)i->usage, VK_SHARING_MODE_EXCLUSIVE, 0, NULL, VK_IMAGE_LAYOUT_UNDEFINED };
    VkImage img = VK_NULL_HANDLE; VkDeviceMemory mem = VK_NULL_HANDLE; VkImageView view = VK_NULL_HANDLE;
    if (nvmtl_typed_CreateImage(g_dev, &ici, NULL, &img) != VK_SUCCESS) return -1;
    VkMemoryRequirements mr; pvkGetImageMemoryRequirements(g_dev, img, &mr);
    int mt = -1;
    if (to_sysmem) {
        for (uint32_t k = 0; k < g_memp.memoryTypeCount; k++)
            if ((mr.memoryTypeBits & (1u << k)) && !(g_memp.memoryTypes[k].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) { mt = (int)k; break; }
    } else {
        if (nvmtl_vk_vram_wall(mr.size, NULL, NULL, NULL, NULL)) { pvkDestroyImage(g_dev, img, NULL); g_res1_refused++; return 1; }
        mt = memtype(mr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    }
    if (mt < 0) { pvkDestroyImage(g_dev, img, NULL); return -1; }
    VkMemoryAllocateInfo mai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, NULL, mr.size, (uint32_t)mt };
    if (pvkAllocateMemory(g_dev, &mai, NULL, &mem) != VK_SUCCESS) { pvkDestroyImage(g_dev, img, NULL); if (!to_sysmem) g_res1_refused++; return to_sysmem ? -1 : 1; }
    if (pvkBindImageMemory(g_dev, img, mem, 0) != VK_SUCCESS) { pvkFreeMemory(g_dev, mem, NULL); pvkDestroyImage(g_dev, img, NULL); return -1; }
    VkComponentMapping swz = a8 ? (VkComponentMapping){ VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_R }
                                : (VkComponentMapping){ 0, 0, 0, 0 };
    VkImageViewType viewType = t == 0 ? VK_IMAGE_VIEW_TYPE_1D : t == 1 ? VK_IMAGE_VIEW_TYPE_1D_ARRAY : t == 3 ? VK_IMAGE_VIEW_TYPE_2D_ARRAY
                             : t == 5 ? VK_IMAGE_VIEW_TYPE_CUBE : t == 6 ? VK_IMAGE_VIEW_TYPE_CUBE_ARRAY : t == 7 ? VK_IMAGE_VIEW_TYPE_3D : VK_IMAGE_VIEW_TYPE_2D;
    VkImageViewCreateInfo vci = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, img, viewType,
        (VkFormat)i->fmt, swz, { VK_IMAGE_ASPECT_COLOR_BIT, 0, i->mips ? i->mips : 1, 0, t == 7 ? 1 : i->layers } };
    if (pvkCreateImageView(g_dev, &vci, NULL, &view) != VK_SUCCESS) { pvkFreeMemory(g_dev, mem, NULL); pvkDestroyImage(g_dev, img, NULL); return -1; }
    if (i->view) pvkDestroyImageView(g_dev, (VkImageView)i->view, NULL);
    pvkDestroyImage(g_dev, (VkImage)i->img, NULL);
    if (i->mem) { pvkFreeMemory(g_dev, (VkDeviceMemory)i->mem, NULL); g_alloc_bytes -= i->alloc; if (i->sysmem) { g_sys_bytes -= i->alloc; g_sys_align -= (i->alloc + 65535ull) & ~65535ull; } else { g_vram_bytes -= i->alloc; g_img_align -= (i->alloc + 65535ull) & ~65535ull; }    }
    uint64_t aligned = (mr.size + 65535ull) & ~65535ull;
    g_alloc_bytes += mr.size;
    if (to_sysmem) { g_sys_bytes += mr.size; g_sys_align += aligned; g_res1_off++; g_res1_off_b += i->alloc; }
    else { g_vram_bytes += mr.size; g_img_align += aligned; nvmtl_vk_budget_charge(mr.size); g_res1_on++; g_res1_on_b += mr.size; }
    i->img = img; i->mem = mem; i->view = view; i->alloc = mr.size; i->sysmem = to_sysmem ? 1 : 0; i->layout = VK_IMAGE_LAYOUT_UNDEFINED;
    return 0;
}

static uint64_t g_res2_off, g_res2_on, g_res2_off_b, g_res2_on_b, g_res2_fail;
void nvmtl_vk_res2_stats(uint64_t out[5]) { out[0] = g_res2_off; out[1] = g_res2_on; out[2] = g_res2_off_b >> 20; out[3] = g_res2_on_b >> 20; out[4] = g_res2_fail; }
int nvmtl_vk_vram_room(uint64_t want) { g_budget_stale = 1; return !nvmtl_vk_vram_wall0(want, NULL, NULL, NULL, NULL); }
uint64_t nvmtl_vk_vram_free_now(void) { uint64_t f = 0; g_budget_stale = 1; (void)nvmtl_vk_vram_wall0(0, &f, NULL, NULL, NULL); return f; }
uint64_t nvmtl_vk_vram_headroom(void) { return NVMTL_VRAM_HEADROOM; }
void nvmtl_vk_bindless_rewrite(uint32_t slot, void *view)
{
    if (!g_heapSet || !slot || slot >= NVMTL_BINDLESS_SLOTS || !view) return;
    pthread_mutex_lock(&g_heap_lock); nvmtl_heap_write_locked(slot, view); pthread_mutex_unlock(&g_heap_lock);
}
void nvmtl_vk_image_free_backing(nvk_image *o)
{
    if (g_state != 1 || !o || !o->img) return;
    if (o->view) pvkDestroyImageView(g_dev, (VkImageView)o->view, NULL);
    pvkDestroyImage(g_dev, (VkImage)o->img, NULL);
    if (o->mem) { pvkFreeMemory(g_dev, (VkDeviceMemory)o->mem, NULL); g_alloc_bytes -= o->alloc; if (o->sysmem) { g_sys_bytes -= o->alloc; g_sys_align -= (o->alloc + 65535ull) & ~65535ull; } else { g_vram_bytes -= o->alloc; g_img_align -= (o->alloc + 65535ull) & ~65535ull; }    }
    memset(o, 0, sizeof *o);
}
static __thread nvk_cmdbuf t_mig_cb; static __thread nvk_cmdbuf *t_mig_batch; static __thread unsigned t_mig_n;
int nvmtl_vk_mig_batch_begin(void)
{
    if (t_mig_batch) return 0;
    if (nvmtl_vk_cmd_begin(NULL, &t_mig_cb)) return -1;
    t_mig_batch = &t_mig_cb; t_mig_n = 0; return 0;
}
int nvmtl_vk_mig_batch_end(void)
{
    nvk_cmdbuf *c = t_mig_batch; if (!c) return 0;
    t_mig_batch = NULL;
    if (!t_mig_n) { nvmtl_vk_cmd_abandon(c); return 0; }
    const unsigned n = t_mig_n; t_mig_n = 0;
    const int r = nvmtl_vk_submit_wait(c);
    if (r) nvlog("residency (res2): evict-batch of %u copies FAILED to complete (%d) - those textures now read undefined contents", n, r);
    return r;
}
int nvmtl_vk_image_migrate(nvk_image *i, int to_sysmem, int a8, nvk_image *old_out)
{
    if (g_state != 1 || !i || !i->img || i->placed || !i->usage || i->vpair || i->samples > 1 || i->mtl_type == 4 || i->mtl_type > 7
        || (i->usage & VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT)) return -1;
    if (!to_sysmem == !i->sysmem) return 1;
    const uint32_t t = i->mtl_type, mips = i->mips ? i->mips : 1, nl = t == 7 ? 1 : (i->layers ? i->layers : 1);
    VkImageCreateInfo ici = { VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, NULL, (VkImageCreateFlags)i->vkflags,
        t == 7 ? VK_IMAGE_TYPE_3D : t < 2 ? VK_IMAGE_TYPE_1D : VK_IMAGE_TYPE_2D, (VkFormat)i->fmt,
        { i->w, i->h, t == 7 ? i->layers : 1 }, mips, nl, VK_SAMPLE_COUNT_1_BIT,
        VK_IMAGE_TILING_OPTIMAL, (VkImageUsageFlags)i->usage, VK_SHARING_MODE_EXCLUSIVE, 0, NULL, VK_IMAGE_LAYOUT_UNDEFINED };
    VkImage img = VK_NULL_HANDLE; VkDeviceMemory mem = VK_NULL_HANDLE; VkImageView view = VK_NULL_HANDLE;
    if (nvmtl_typed_CreateImage(g_dev, &ici, NULL, &img) != VK_SUCCESS) return -1;
    VkMemoryRequirements mr; pvkGetImageMemoryRequirements(g_dev, img, &mr);
    int mt = -1;
    if (to_sysmem) {
        for (uint32_t k = 0; k < g_memp.memoryTypeCount; k++)
            if ((mr.memoryTypeBits & (1u << k)) && !(g_memp.memoryTypes[k].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) { mt = (int)k; break; }
    } else {
        if (nvmtl_vk_vram_wall0(mr.size, NULL, NULL, NULL, NULL)) { pvkDestroyImage(g_dev, img, NULL); return 1; }
        mt = memtype(mr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    }
    if (mt < 0) { pvkDestroyImage(g_dev, img, NULL); return -1; }
    VkMemoryAllocateInfo mai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, NULL, mr.size, (uint32_t)mt };
    if (pvkAllocateMemory(g_dev, &mai, NULL, &mem) != VK_SUCCESS) { pvkDestroyImage(g_dev, img, NULL); return to_sysmem ? -1 : 1; }
    if (pvkBindImageMemory(g_dev, img, mem, 0) != VK_SUCCESS) goto fail;
    { VkComponentMapping swz = a8 ? (VkComponentMapping){ VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_R }
                                  : (VkComponentMapping){ 0, 0, 0, 0 };
      VkImageViewType vt = t == 0 ? VK_IMAGE_VIEW_TYPE_1D : t == 1 ? VK_IMAGE_VIEW_TYPE_1D_ARRAY : t == 3 ? VK_IMAGE_VIEW_TYPE_2D_ARRAY
                         : t == 5 ? VK_IMAGE_VIEW_TYPE_CUBE : t == 6 ? VK_IMAGE_VIEW_TYPE_CUBE_ARRAY : t == 7 ? VK_IMAGE_VIEW_TYPE_3D : VK_IMAGE_VIEW_TYPE_2D;
      VkImageViewCreateInfo vci = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, img, vt, (VkFormat)i->fmt, swz,
                                    { VK_IMAGE_ASPECT_COLOR_BIT, 0, mips, 0, nl } };
      if (pvkCreateImageView(g_dev, &vci, NULL, &view) != VK_SUCCESS) goto fail; }
    const VkImageLayout was = (VkImageLayout)i->layout;
    if (was != VK_IMAGE_LAYOUT_UNDEFINED) {
        nvk_cmdbuf c0, *cp = t_mig_batch; if (!cp) { if (nvmtl_vk_cmd_begin(NULL, &c0)) goto fail; cp = &c0; }
        const VkImageSubresourceRange rr = { VK_IMAGE_ASPECT_COLOR_BIT, 0, mips, 0, nl };
        VkImageMemoryBarrier b[2] = {
            { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_TRANSFER_READ_BIT,
              was, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)i->img, rr },
            { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, 0, VK_ACCESS_TRANSFER_WRITE_BIT,
              VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, img, rr } };
        pvkCmdPipelineBarrier(cp->cb, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 2, b);
        VkImageCopy reg[16]; const uint32_t nm = mips > 16 ? 16 : mips;
        for (uint32_t m = 0; m < nm; m++) {
            const uint32_t w = i->w >> m ? i->w >> m : 1, h = i->h >> m ? i->h >> m : 1, d = t == 7 ? (i->layers >> m ? i->layers >> m : 1) : 1;
            reg[m] = (VkImageCopy){ { VK_IMAGE_ASPECT_COLOR_BIT, m, 0, nl }, { 0, 0, 0 }, { VK_IMAGE_ASPECT_COLOR_BIT, m, 0, nl }, { 0, 0, 0 }, { w, h, d } };
        }
        pvkCmdCopyImage(cp->cb, (VkImage)i->img, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, nm, reg);
        VkImageMemoryBarrier a = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, VK_ACCESS_TRANSFER_WRITE_BIT,
            VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, was,
            VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, img, rr };
        pvkCmdPipelineBarrier(cp->cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, 0, 0, NULL, 0, NULL, 1, &a);
        if (cp == &c0) { if (nvmtl_vk_submit_wait(&c0)) goto fail; }
        else t_mig_n++;
    }
    *old_out = *i;
    { uint64_t aligned = (mr.size + 65535ull) & ~65535ull;
      g_alloc_bytes += mr.size;
      if (to_sysmem) { g_sys_bytes += mr.size; g_sys_align += aligned; g_res2_off++; g_res2_off_b += i->alloc; }
      else { g_vram_bytes += mr.size; g_img_align += aligned; nvmtl_vk_budget_charge(mr.size); g_res2_on++; g_res2_on_b += mr.size; } }
    i->img = img; i->mem = mem; i->view = view; i->alloc = mr.size; i->sysmem = to_sysmem ? 1 : 0; i->layout = (uint32_t)was;
    return 0;
fail:
    if (view) pvkDestroyImageView(g_dev, view, NULL);
    pvkDestroyImage(g_dev, img, NULL);
    pvkFreeMemory(g_dev, mem, NULL);
    g_res2_fail++;
    return -1;
}

int nvmtl_vk_buffer_migrate(nvk_buffer *b, int to_sysmem)
{
    if (g_state != 1 || !b || !b->sparse || !b->buf || !b->mem) return -1;
    if (!to_sysmem == !b->sysmem) return 1;
    VkMemoryRequirements mr; pvkGetBufferMemoryRequirements(g_dev, (VkBuffer)b->buf, &mr);
    int mt;
    if (to_sysmem) mt = nvmtl_vk_sysmem_type(mr.memoryTypeBits);
    else { if (nvmtl_vk_vram_wall0(mr.size, NULL, NULL, NULL, NULL)) return 1; mt = memtype(mr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT); }
    if (mt < 0) return -1;
    VkMemoryAllocateFlagsInfo mafi = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO, NULL, VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT, 0 };
    VkMemoryAllocateInfo mai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &mafi, mr.size, (uint32_t)mt };
    VkDeviceMemory nm = VK_NULL_HANDLE;
    if (pvkAllocateMemory(g_dev, &mai, NULL, &nm) != VK_SUCCESS) return to_sysmem ? -1 : 1;
    VkBufferCreateInfo tci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, 0, mr.size, VK_BUFFER_USAGE_TRANSFER_DST_BIT,
                               VK_SHARING_MODE_EXCLUSIVE, 0, NULL };
    VkBuffer tmp = VK_NULL_HANDLE;
    if (pvkCreateBuffer(g_dev, &tci, NULL, &tmp) != VK_SUCCESS) { pvkFreeMemory(g_dev, nm, NULL); return -1; }
    if (pvkBindBufferMemory(g_dev, tmp, nm, 0) != VK_SUCCESS) { pvkDestroyBuffer(g_dev, tmp, NULL); pvkFreeMemory(g_dev, nm, NULL); return -1; }
    { nvk_cmdbuf c; if (nvmtl_vk_cmd_begin(NULL, &c)) { pvkDestroyBuffer(g_dev, tmp, NULL); pvkFreeMemory(g_dev, nm, NULL); return -1; }
      VkMemoryBarrier pre = { VK_STRUCTURE_TYPE_MEMORY_BARRIER, NULL, VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_TRANSFER_READ_BIT };
      pvkCmdPipelineBarrier(c.cb, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 1, &pre, 0, NULL, 0, NULL);
      VkBufferCopy bc = { 0, 0, b->size < mr.size ? b->size : mr.size };
      pvkCmdCopyBuffer(c.cb, (VkBuffer)b->buf, tmp, 1, &bc);
      VkMemoryBarrier post = { VK_STRUCTURE_TYPE_MEMORY_BARRIER, NULL, VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT };
      pvkCmdPipelineBarrier(c.cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, 0, 1, &post, 0, NULL, 0, NULL);
      if (nvmtl_vk_submit_wait(&c)) { pvkDestroyBuffer(g_dev, tmp, NULL); pvkFreeMemory(g_dev, nm, NULL); return -1; } }
    pvkDestroyBuffer(g_dev, tmp, NULL);
    if (nvmtl_vk_sparse_bind((VkBuffer)b->buf, 1, nm, mr.size)) {
        if (nvmtl_vk_sparse_bind((VkBuffer)b->buf, 0, (VkDeviceMemory)b->mem, mr.size))
            nvlog("residency (res3): a %llu MB buffer is LEFT WITHOUT BACKING (rebind and restore both refused) - its GPU address will fault",
                  (unsigned long long)(mr.size >> 20));
        pvkFreeMemory(g_dev, nm, NULL);
        return -1;
    }
    pvkFreeMemory(g_dev, (VkDeviceMemory)b->mem, NULL);
    g_alloc_bytes -= b->alloc; g_alloc_bytes += mr.size;
    if (to_sysmem) { g_res3_off++; g_res3_off_b += b->alloc; }
    else { nvmtl_vk_budget_charge(mr.size); g_res3_on++; g_res3_on_b += mr.size; }
    b->mem = nm; b->alloc = mr.size; b->sysmem = to_sysmem ? 1 : 0;
    return 0;
}

void nvmtl_vk_queue_destroy(nvk_queue *q)
{
    if (g_state != 1 || !q) return;
    nvmtl_vk_buffer_destroy(&q->stage);
    if (q->pool) pvkDestroyCommandPool(g_dev, (VkCommandPool)q->pool, NULL);
    memset(q, 0, sizeof *q);
}

size_t nvmtl_vk_allocated_bytes(void) { return g_alloc_bytes; }

int nvmtl_vk_image_write(nvk_queue *q, nvk_image *img, const void *src, size_t row_bytes)
{
    const size_t bpp = img->bpp ? img->bpp : 4;
    const uint32_t cols = nvmtl_blocks(img->w, img->bw), rows = nvmtl_blocks(img->h, img->bh);
    if (row_bytes < (size_t)cols * bpp) { nvlog("image_write: bytesPerRow %zu < %u blocks * %zu B", row_bytes, cols, bpp); return -1; }
    nvk_buffer stage;
    if (nvmtl_vk_stage(q, (size_t)cols * rows * bpp, &stage)) return -1;
    uint8_t *dst = (uint8_t *)stage.map;
    for (uint32_t y = 0; y < rows; y++)
        memcpy(dst + (size_t)y * cols * bpp, (const uint8_t *)src + (size_t)y * row_bytes, (size_t)cols * bpp);
    nvk_cmdbuf c; if (nvmtl_vk_cmd_begin(q, &c)) return -1;
    VkImageSubresourceRange range = { nvmtl_barrier_aspect(img), 0, 1, 0, 1 };
    VkImageMemoryBarrier toDst = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, 0, VK_ACCESS_TRANSFER_WRITE_BIT,
        VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)img->img, range };
    pvkCmdPipelineBarrier(c.cb, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &toDst);
    VkBufferImageCopy bic = { 0, 0, 0, { nvmtl_copy_aspect(img), 0, 0, 1 }, { 0, 0, 0 }, { img->w, img->h, 1 } };
    pvkCmdCopyBufferToImage(c.cb, (VkBuffer)stage.buf, (VkImage)img->img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &bic);
    VkImageMemoryBarrier toRead = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL,
        VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_SHADER_READ_BIT,
        VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)img->img, range };
    pvkCmdPipelineBarrier(c.cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, 0, 0, NULL, 0, NULL, 1, &toRead);
    img->layout = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
    return nvmtl_vk_submit_wait(&c);
}

int nvmtl_vk_copy_buffer(nvk_queue *q, nvk_buffer *src, size_t srcOff, nvk_buffer *dst, size_t dstOff, size_t size)
{
    const uint64_t t0 = nvmtl_perf_now();
    nvk_cmdbuf c; if (nvmtl_vk_cmd_begin(q, &c)) return -1;
    if (nvmtl_vk_cmd_copy_buffer(&c, src, srcOff, dst, dstOff, size)) { nvmtl_vk_cmd_abandon(&c); return -1; }
    const int k0 = g_perf_kind; g_perf_kind |= 2;
    const int r = nvmtl_vk_submit_wait(&c);
    g_perf_kind = k0;
    nvmtl_perf_note(NVP_COPY, t0, size);
    return r;
}

int nvmtl_vk_image_read(nvk_queue *q, nvk_image *img, void *dst, size_t row_bytes)
{
    const size_t bpp = img->bpp ? img->bpp : 4;
    const uint32_t cols = nvmtl_blocks(img->w, img->bw), rows = nvmtl_blocks(img->h, img->bh);
    if (row_bytes < (size_t)cols * bpp) { nvlog("image_read: bytesPerRow %zu < %u blocks * %zu B", row_bytes, cols, bpp); return -1; }
    nvk_buffer stage;
    if (nvmtl_vk_stage(q, (size_t)cols * rows * bpp, &stage)) return -1;
    nvk_cmdbuf c; if (nvmtl_vk_cmd_begin(q, &c)) return -1;
    VkBufferImageCopy bic = { 0, 0, 0, { nvmtl_copy_aspect(img), 0, 0, 1 }, { 0, 0, 0 }, { img->w, img->h, 1 } };
    pvkCmdCopyImageToBuffer(c.cb, img->img, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, stage.buf, 1, &bic);
    if (nvmtl_vk_submit_wait(&c)) return -1;
    const uint8_t *src = (const uint8_t *)stage.map;
    for (uint32_t y = 0; y < rows; y++) memcpy((uint8_t *)dst + y * row_bytes, src + (size_t)y * cols * bpp, (size_t)cols * bpp);
    return 0;
}

static void img_to(nvk_cmdbuf *c, nvk_image *img, VkImageLayout to,
                   VkAccessFlags srcA, VkAccessFlags dstA, VkPipelineStageFlags srcS, VkPipelineStageFlags dstS)
{
    if ((VkImageLayout)img->layout == to) return;
    VkImageSubresourceRange r = { nvmtl_barrier_aspect(img), 0, img->mips ? img->mips : 1, 0, img->layers ? img->layers : 1 };
    VkImageMemoryBarrier b = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, srcA, dstA,
        (VkImageLayout)img->layout, to, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)img->img, r };
    nvmtl_sample_pattern pattern = c->sample_pattern;
    VkSampleLocationEXT positions[8]; VkSampleLocationsInfoEXT locations;
    if (g_sample_locations && (r.aspectMask & VK_IMAGE_ASPECT_DEPTH_BIT)) {
        if (pattern.count != img->samples) nvmtl_sample_default(img->samples, &pattern);
        locations = nvmtl_sample_info(&pattern, positions);

        b.pNext = &locations;
    }
    pvkCmdPipelineBarrier(c->cb, srcS, dstS, 0, 0, NULL, 0, NULL, 1, &b);
    img->layout = to;
}

static void nvmtl_private_depth_layout(nvk_cmdbuf *c, nvk_image *img, VkImageLayout to)
{
    VkSampleLocationEXT positions[8]; nvmtl_sample_pattern pattern;
    if (nvmtl_sample_default(img->samples ? img->samples : 1, &pattern)) return;
    VkSampleLocationsInfoEXT locations = nvmtl_sample_info(&pattern, positions);
    VkImageMemoryBarrier b = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, &locations,
        VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT,
        VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT,
        VK_IMAGE_LAYOUT_UNDEFINED, to, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED,
        (VkImage)img->img, {nvmtl_barrier_aspect(img), 0, img->mips ? img->mips : 1, 0, img->layers ? img->layers : 1} };
    pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, 0, 0, NULL, 0, NULL, 1, &b);
    img->layout = to;
}
int nvmtl_vk_cmd_prepare_sampled_image(nvk_cmdbuf *c, nvk_image *img, int storage)
{
    if (!c || !img || c->in_rp) return -1;
    const VkImageLayout to = storage ? VK_IMAGE_LAYOUT_GENERAL : VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
    if (g_sample_locations && (nvmtl_barrier_aspect(img) & VK_IMAGE_ASPECT_DEPTH_BIT))
        nvmtl_private_depth_layout(c, img, to);
    else img_to(c, img, to, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT,
                VK_ACCESS_SHADER_READ_BIT | (storage ? VK_ACCESS_SHADER_WRITE_BIT : 0),
                VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT);
    return 0;
}

int nvmtl_vk_image_write_region_level(nvk_queue *q, nvk_image *img, const void *src, size_t row_bytes,
                                      uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t level)
{ return nvmtl_vk_image_write_region_level_layer(q, img, src, row_bytes, x, y, w, h, level, 0); }
int nvmtl_vk_image_write_region_level_layer(nvk_queue *q, nvk_image *img, const void *src, size_t row_bytes,
                                      uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t level, uint32_t layer)
{
    if (layer >= (img->layers ? img->layers : 1)) { nvlog("image_write_region: layer %u — this image has %u", layer, img->layers ? img->layers : 1); return -1; }
    { const uint32_t lw = img->w >> level ? img->w >> level : 1, lh = img->h >> level ? img->h >> level : 1;
      if (!w || !h || x + w > lw || y + h > lh) { nvlog("image_write_region: %ux%u at (%u,%u) is outside %ux%u (level %u)", w, h, x, y, lw, lh, level); return -1; } }
    const size_t bpp = img->bpp ? img->bpp : 4;
    const uint32_t cols = nvmtl_blocks(w, img->bw), rows = nvmtl_blocks(h, img->bh);
    if (img->bw > 1 && ((x % img->bw) || (y % img->bh))) { nvlog("image_write_region: origin (%u,%u) is not block-aligned (%ux%u blocks)", x, y, img->bw, img->bh); return -1; }
    if (row_bytes < (size_t)cols * bpp) { nvlog("image_write_region: bytesPerRow %zu < %u blocks * %zu B", row_bytes, cols, bpp); return -1; }
    nvk_buffer stage;
    if (nvmtl_vk_stage(q, (size_t)cols * rows * bpp, &stage)) return -1;
    uint8_t *dst = (uint8_t *)stage.map;
    for (uint32_t r = 0; r < rows; r++) memcpy(dst + (size_t)r * cols * bpp, (const uint8_t *)src + (size_t)r * row_bytes, (size_t)cols * bpp);
    nvk_cmdbuf c; if (nvmtl_vk_cmd_begin(q, &c)) return -1;
    img_to(&c, img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 0, VK_ACCESS_TRANSFER_WRITE_BIT,
           VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
    VkBufferImageCopy bic = { 0, 0, 0, { nvmtl_copy_aspect(img), level, img->mtl_type == 7 ? 0u : layer, 1 }, { (int32_t)x, (int32_t)y, img->mtl_type == 7 ? (int32_t)layer : 0 }, { w, h, 1 } };
    pvkCmdCopyBufferToImage(c.cb, (VkBuffer)stage.buf, (VkImage)img->img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &bic);
    img_to(&c, img, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_SHADER_READ_BIT,
           VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT);
    return nvmtl_vk_submit_wait(&c);
}

int nvmtl_vk_image_read_region_level(nvk_queue *q, nvk_image *img, void *dst, size_t row_bytes,
                                     uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t level)
{ return nvmtl_vk_image_read_region_level_layer(q, img, dst, row_bytes, x, y, w, h, level, 0); }
int nvmtl_vk_image_read_region_level_layer(nvk_queue *q, nvk_image *img, void *dst, size_t row_bytes,
                                     uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t level, uint32_t layer)
{
    if (layer >= (img->layers ? img->layers : 1)) { nvlog("image_read_region: layer %u — this image has %u", layer, img->layers ? img->layers : 1); return -1; }
    { const uint32_t lw = img->w >> level ? img->w >> level : 1, lh = img->h >> level ? img->h >> level : 1;
      if (!w || !h || x + w > lw || y + h > lh) { nvlog("image_read_region: %ux%u at (%u,%u) is outside %ux%u (level %u)", w, h, x, y, lw, lh, level); return -1; } }
    const size_t bpp = img->bpp ? img->bpp : 4;
    const uint32_t cols = nvmtl_blocks(w, img->bw), rows = nvmtl_blocks(h, img->bh);
    if (img->bw > 1 && ((x % img->bw) || (y % img->bh))) { nvlog("image_read_region: origin (%u,%u) is not block-aligned", x, y); return -1; }
    if (row_bytes < (size_t)cols * bpp) { nvlog("image_read_region: bytesPerRow %zu < %u blocks * %zu B", row_bytes, cols, bpp); return -1; }
    nvk_buffer stage;
    if (nvmtl_vk_stage(q, (size_t)cols * rows * bpp, &stage)) return -1;
    nvk_cmdbuf c; if (nvmtl_vk_cmd_begin(q, &c)) return -1;
    img_to(&c, img, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_ACCESS_SHADER_READ_BIT, VK_ACCESS_TRANSFER_READ_BIT,
           VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
    VkBufferImageCopy bic = { 0, 0, 0, { nvmtl_copy_aspect(img), level, img->mtl_type == 7 ? 0u : layer, 1 }, { (int32_t)x, (int32_t)y, img->mtl_type == 7 ? (int32_t)layer : 0 }, { w, h, 1 } };
    pvkCmdCopyImageToBuffer(c.cb, (VkImage)img->img, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, (VkBuffer)stage.buf, 1, &bic);
    if (nvmtl_vk_submit_wait(&c)) return -1;
    const uint8_t *src = (const uint8_t *)stage.map;
    for (uint32_t r = 0; r < rows; r++) memcpy((uint8_t *)dst + (size_t)r * row_bytes, src + (size_t)r * cols * bpp, (size_t)cols * bpp);
    return 0;
}

int nvmtl_vk_copy_image(nvk_queue *q, nvk_image *src, uint32_t sx, uint32_t sy,
                        nvk_image *dst, uint32_t dx, uint32_t dy, uint32_t w, uint32_t h)
{
    nvk_cmdbuf c; if (nvmtl_vk_cmd_begin(q, &c)) return -1;
    if (nvmtl_vk_cmd_copy_image(&c, src, sx, sy, dst, dx, dy, w, h)) { nvmtl_vk_cmd_abandon(&c); return -1; }
    return nvmtl_vk_submit_wait(&c);
}

int nvmtl_vk_copy_buffer_to_image(nvk_queue *q, nvk_buffer *b, size_t off, uint32_t row_bytes,
                                  nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h)
{
    return nvmtl_vk_copy_buffer_to_image_layer(q, b, off, row_bytes, img, x, y, w, h, 0);
}
int nvmtl_vk_copy_buffer_to_image_layer(nvk_queue *q, nvk_buffer *b, size_t off, uint32_t row_bytes,
                                  nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t layer)
{
    nvk_cmdbuf c; if (nvmtl_vk_cmd_begin(q, &c)) return -1;
    if (nvmtl_vk_cmd_copy_buffer_to_image_layer(&c, b, off, row_bytes, img, x, y, w, h, layer)) { nvmtl_vk_cmd_abandon(&c); return -1; }
    return nvmtl_vk_submit_wait(&c);
}

int nvmtl_vk_copy_image_to_buffer(nvk_queue *q, nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h,
                                  nvk_buffer *b, size_t off, uint32_t row_bytes)
{
    return nvmtl_vk_copy_image_to_buffer_layer(q, img, x, y, w, h, b, off, row_bytes, 0);
}
int nvmtl_vk_copy_image_to_buffer_layer(nvk_queue *q, nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h,
                                  nvk_buffer *b, size_t off, uint32_t row_bytes, uint32_t layer)
{
    nvk_cmdbuf c; if (nvmtl_vk_cmd_begin(q, &c)) return -1;
    if (nvmtl_vk_cmd_copy_image_to_buffer_layer(&c, img, x, y, w, h, b, off, row_bytes, layer)) { nvmtl_vk_cmd_abandon(&c); return -1; }
    return nvmtl_vk_submit_wait(&c);
}

void nvmtl_vk_cmd_draw_indirect(nvk_cmdbuf *c, nvk_buffer *b, size_t off)
{ if (c->nobind) return; pvkCmdDrawIndirect(c->cb, (VkBuffer)b->buf, off, 1, 0); }
int nvmtl_vk_cmd_draw_patches(nvk_cmdbuf *c, const nvk_pipeline *p, uint64_t fac, uint32_t istride, float scale, uint64_t cpx, uint64_t pix,
                              uint32_t start, uint32_t count, uint32_t instances, uint32_t baseInstance, nvk_buffer *ind, size_t indoff)
{
    if (c->nobind) return 0;
    if (!p || !p->tess || !fac) return -1;
    struct { uint64_t fac, cpx, pix; uint32_t istride; float scale; uint64_t vb[NVMTL_TESS_NVB]; } pc;
    _Static_assert(sizeof pc == NVMTL_TESS_PC_BYTES, "the TESC's push-constant block is 96 bytes, offsets 0/8/16/24/28/32");
    memset(&pc, 0, sizeof pc); pc.fac = fac; pc.cpx = cpx; pc.pix = pix; pc.istride = istride; pc.scale = scale;
    nvmtl_bindtable *t = (nvmtl_bindtable *)c->table;
    for (uint32_t k = 0; k < p->tess_nvb && k < NVMTL_TESS_NVB; k++) {
        uint32_t i = p->tess_vb[k]; nvmtl_slot *sl = t && i < NVMTL_NVIN ? &t->s[0][i] : NULL;
        if (!sl || sl->kind != NVMTL_SLOT_BUF || !sl->h || !sl->address) { static int said; if (said++ < 8) nvlog("tess draw: the pipeline pulls "
            "control points from vertex buffer index %u and nothing is bound there — NOT drawn", i); return -1; }
        pc.vb[k] = sl->address + sl->off;
        if (pc.vb[k] & 3) { static int said2; if (said2++ < 8) nvlog("tess draw: vertex buffer index %u is bound at an offset that is not "
            "4-byte aligned — NOT drawn (the TESC loads whole words)", i); return -1; }
    }
    pvkCmdPushConstants(c->cb, (VkPipelineLayout)p->layout, VK_SHADER_STAGE_TESSELLATION_CONTROL_BIT, 0, sizeof pc, &pc);
    pvkCmdSetPrimitiveTopology(c->cb, VK_PRIMITIVE_TOPOLOGY_PATCH_LIST);
    pvkCmdSetPrimitiveRestartEnable(c->cb, VK_FALSE);
    if (ind) pvkCmdDrawIndirect(c->cb, (VkBuffer)ind->buf, indoff, 1, 0);
    else pvkCmdDraw(c->cb, count, instances ? instances : 1, start, baseInstance);
    { static int told; if (!told++) nvlog("tess: first patch draw in this process (%s)", ind ? "indirect" : "direct"); }
    return 0;
}
void nvmtl_vk_cmd_draw_indexed_indirect(nvk_cmdbuf *c, nvk_buffer *idx, size_t idxOff, int is32, nvk_buffer *b, size_t off)
{
    pvkCmdBindIndexBuffer(c->cb, (VkBuffer)idx->buf, idxOff, is32 ? VK_INDEX_TYPE_UINT32 : VK_INDEX_TYPE_UINT16);
    if (c->nobind) return;
    pvkCmdDrawIndexedIndirect(c->cb, (VkBuffer)b->buf, off, 1, 0);
}
void nvmtl_vk_cmd_dispatch_indirect(nvk_cmdbuf *c, nvk_pipeline *p, nvk_buffer *b, size_t off)
{
    pvkCmdBindPipeline(c->cb, VK_PIPELINE_BIND_POINT_COMPUTE, (VkPipeline)p->pipe);
    if (c->nobind) return;
    pvkCmdDispatchIndirect(c->cb, (VkBuffer)b->buf, off);
}

void nvmtl_vk_cmd_barrier(nvk_cmdbuf *c)
{
    VkMemoryBarrier mb = { VK_STRUCTURE_TYPE_MEMORY_BARRIER, NULL,
        VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT };
    pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                          0, 1, &mb, 0, NULL, 0, NULL);
}
void nvmtl_vk_cmd_barrier_c2c(nvk_cmdbuf *c)
{
    VkMemoryBarrier mb = { VK_STRUCTURE_TYPE_MEMORY_BARRIER, NULL,
        VK_ACCESS_SHADER_WRITE_BIT, VK_ACCESS_SHADER_READ_BIT | VK_ACCESS_SHADER_WRITE_BIT | VK_ACCESS_UNIFORM_READ_BIT };
    pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT, VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,
                          0, 1, &mb, 0, NULL, 0, NULL);
}

int nvmtl_vk_rt_available(void) { return nvmtl_vk_init() ? 0 : g_rt; }

static uint32_t nvmtl_fill_geometries(const nvmtl_geom *g, uint32_t n, uint64_t instances_addr, uint32_t instance_count, int instance,
                                      VkAccelerationStructureGeometryKHR *out, uint32_t *counts)
{
    if (instance) {
        out[0] = (VkAccelerationStructureGeometryKHR){ VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_GEOMETRY_KHR, NULL, VK_GEOMETRY_TYPE_INSTANCES_KHR };
        out[0].geometry.instances = (VkAccelerationStructureGeometryInstancesDataKHR){
            VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_GEOMETRY_INSTANCES_DATA_KHR, NULL, VK_FALSE, { .deviceAddress = instances_addr } };
        counts[0] = instance_count;
        return 1;
    }
    for (uint32_t i = 0; i < n; i++) {
        out[i] = (VkAccelerationStructureGeometryKHR){ VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_GEOMETRY_KHR, NULL,
                                                        g[i].is_boxes ? VK_GEOMETRY_TYPE_AABBS_KHR : VK_GEOMETRY_TYPE_TRIANGLES_KHR };
        if (g[i].is_boxes)
            out[i].geometry.aabbs = (VkAccelerationStructureGeometryAabbsDataKHR){
                VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_GEOMETRY_AABBS_DATA_KHR, NULL, { .deviceAddress = g[i].boxes }, g[i].box_stride };
        else
            out[i].geometry.triangles = (VkAccelerationStructureGeometryTrianglesDataKHR){
                VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_GEOMETRY_TRIANGLES_DATA_KHR, NULL, (VkFormat)g[i].vertex_format,
                { .deviceAddress = g[i].vertices }, g[i].stride, g[i].max_vertex, (VkIndexType)g[i].index_type,
                { .deviceAddress = g[i].indices }, { .deviceAddress = 0 } };
        out[i].flags = g[i].opaque ? VK_GEOMETRY_OPAQUE_BIT_KHR : 0;
        counts[i] = g[i].count;
    }
    return n;
}

int nvmtl_vk_accel_sizes(const nvmtl_geom *g, uint32_t n, int instance, uint32_t instance_count, size_t *as_size, size_t *scratch_size)
{
    if (nvmtl_vk_init() || !g_rt) return -1;
    if (n > NVMTL_MAX_GEOMS) { nvlog("vk: %u geometries in one acceleration structure — more than %d carried", n, NVMTL_MAX_GEOMS); return -1; }
    static VkAccelerationStructureGeometryKHR geoms[NVMTL_MAX_GEOMS]; static uint32_t counts[NVMTL_MAX_GEOMS];
    uint32_t ng = nvmtl_fill_geometries(g, n, 0, instance_count, instance, geoms, counts);
    VkAccelerationStructureBuildGeometryInfoKHR bi = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_BUILD_GEOMETRY_INFO_KHR, NULL,
        instance ? VK_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL_KHR : VK_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL_KHR,
        VK_BUILD_ACCELERATION_STRUCTURE_PREFER_FAST_TRACE_BIT_KHR, VK_BUILD_ACCELERATION_STRUCTURE_MODE_BUILD_KHR,
        VK_NULL_HANDLE, VK_NULL_HANDLE, ng, geoms, NULL, { .deviceAddress = 0 } };
    VkAccelerationStructureBuildSizesInfoKHR sz = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_BUILD_SIZES_INFO_KHR };
    pvkGetAccelerationStructureBuildSizesKHR(g_dev, VK_ACCELERATION_STRUCTURE_BUILD_TYPE_DEVICE_KHR, &bi, counts, &sz);
    *as_size = sz.accelerationStructureSize; *scratch_size = sz.buildScratchSize;
    return 0;
}

int nvmtl_vk_accel_create(nvk_buffer *backing, size_t size, int instance, nvk_accel *out)
{
    if (nvmtl_vk_init() || !g_rt) return -1;
    memset(out, 0, sizeof *out);
    VkAccelerationStructureCreateInfoKHR ci = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_CREATE_INFO_KHR, NULL, 0, (VkBuffer)backing->buf, 0, size,
        instance ? VK_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL_KHR : VK_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL_KHR, 0 };
    VkAccelerationStructureKHR as;
    VKCK(pvkCreateAccelerationStructureKHR(g_dev, &ci, NULL, &as), "vkCreateAccelerationStructureKHR");
    VkAccelerationStructureDeviceAddressInfoKHR ai = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_DEVICE_ADDRESS_INFO_KHR, NULL, as };
    out->as = as; out->addr = pvkGetAccelerationStructureDeviceAddressKHR(g_dev, &ai); out->size = size; out->instance = instance;
    return 0;
}

void nvmtl_vk_accel_destroy(nvk_accel *a)
{
    if (a->as && pvkDestroyAccelerationStructureKHR) pvkDestroyAccelerationStructureKHR(g_dev, (VkAccelerationStructureKHR)a->as, NULL);
    a->as = NULL;
}

int nvmtl_vk_cmd_build_accel(nvk_cmdbuf *c, nvk_accel *dst, const nvmtl_geom *g, uint32_t n, uint64_t instances_addr, uint32_t instance_count, uint64_t scratch_addr)
{
    if (!g_rt || !dst->as) return -1;
    if (n > NVMTL_MAX_GEOMS) return -1;
    static VkAccelerationStructureGeometryKHR geoms[NVMTL_MAX_GEOMS]; static uint32_t counts[NVMTL_MAX_GEOMS];
    static VkAccelerationStructureBuildRangeInfoKHR ranges[NVMTL_MAX_GEOMS];
    uint32_t ng = nvmtl_fill_geometries(g, n, instances_addr, instance_count, dst->instance, geoms, counts);
    for (uint32_t i = 0; i < ng; i++) ranges[i] = (VkAccelerationStructureBuildRangeInfoKHR){ counts[i], 0, 0, 0 };
    VkAccelerationStructureBuildGeometryInfoKHR bi = { VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_BUILD_GEOMETRY_INFO_KHR, NULL,
        dst->instance ? VK_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL_KHR : VK_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL_KHR,
        VK_BUILD_ACCELERATION_STRUCTURE_PREFER_FAST_TRACE_BIT_KHR, VK_BUILD_ACCELERATION_STRUCTURE_MODE_BUILD_KHR,
        VK_NULL_HANDLE, (VkAccelerationStructureKHR)dst->as, ng, geoms, NULL, { .deviceAddress = scratch_addr } };
    const VkAccelerationStructureBuildRangeInfoKHR *rp = ranges;
    pvkCmdBuildAccelerationStructuresKHR((VkCommandBuffer)c->cb, 1, &bi, &rp);
    nvmtl_vk_cmd_barrier(c);
    return 0;
}

int nvmtl_vk_cmd_copy_accel(nvk_cmdbuf *c, nvk_accel *src, nvk_accel *dst)
{
    if (!g_rt || !src->as || !dst->as || !pvkCmdCopyAccelerationStructureKHR) return -1;
    VkCopyAccelerationStructureInfoKHR ci = { VK_STRUCTURE_TYPE_COPY_ACCELERATION_STRUCTURE_INFO_KHR, NULL,
        (VkAccelerationStructureKHR)src->as, (VkAccelerationStructureKHR)dst->as, VK_COPY_ACCELERATION_STRUCTURE_MODE_CLONE_KHR };
    pvkCmdCopyAccelerationStructureKHR((VkCommandBuffer)c->cb, &ci);
    nvmtl_vk_cmd_barrier(c);
    return 0;
}

int nvmtl_vk_convert_instances(const void *mtl, uint32_t stride, uint32_t count, const uint64_t *as_addrs, uint32_t n_as, void *out,
                               uint32_t mtl_type, uint32_t *uid_out, const uint32_t *as_ift)
{
    for (uint32_t i = 0; i < count; i++) {
        const uint8_t *src = (const uint8_t *)mtl + (size_t)i * stride;
        const float *m = (const float *)src; const uint32_t *w = (const uint32_t *)(src + 48);
        VkAccelerationStructureInstanceKHR *d = (VkAccelerationStructureInstanceKHR *)out + i;
        memset(d, 0, sizeof *d);
        for (int r = 0; r < 3; r++) for (int col = 0; col < 4; col++) d->transform.matrix[r][col] = m[col * 3 + r];
        uint32_t options = w[0], mask = w[1], ift = w[2], as_index = w[3];
        uint32_t user = mtl_type == 1 ? w[4] : mtl_type == 3 ? w[3] : 0;
        if (uid_out) uid_out[i] = user;
        d->instanceCustomIndex = user & 0xFFFFFF;
        d->mask = mask & 0xFF;
        const uint32_t add = (mtl_type != 3 && as_ift && as_index < n_as) ? as_ift[as_index] : 0;
        uint64_t slot = (uint64_t)ift + (add & 0x7FFFFFu);
        if (slot > 0x7FFFFFu) { static unsigned said; if (said++ < 4)
            nvlog("vk: instance %u intersection function table slot %llu is past 0x7FFFFF - the table reads slot %u", i, (unsigned long long)slot, ift & 0x7FFFFFu);
            slot = ift & 0x7FFFFFu; }
        d->instanceShaderBindingTableRecordOffset = (uint32_t)slot | (add & 0x800000u);
        d->flags = options & 0xF;
        if (mtl_type == 3) {
            uint64_t ref; memcpy(&ref, src + 64, 8);
            d->accelerationStructureReference = ref;
            if (!ref) nvlog("vk: indirect instance %u names a nil structure - it is inactive", i);
        } else if (as_index < n_as) d->accelerationStructureReference = as_addrs[as_index];
        else { d->accelerationStructureReference = 0; nvlog("vk: instance %u names structure %u of %u — dropped", i, as_index, n_as); }
    }
    return 0;
}

int nvmtl_vk_image_write_region(nvk_queue *q, nvk_image *img, const void *src, size_t row_bytes,
                                uint32_t x, uint32_t y, uint32_t w, uint32_t h)
{ return nvmtl_vk_image_write_region_level(q, img, src, row_bytes, x, y, w, h, 0); }
int nvmtl_vk_image_read_region(nvk_queue *q, nvk_image *img, void *dst, size_t row_bytes,
                               uint32_t x, uint32_t y, uint32_t w, uint32_t h)
{ return nvmtl_vk_image_read_region_level(q, img, dst, row_bytes, x, y, w, h, 0); }

int nvmtl_vk_image_gen_mipmaps(nvk_queue *q, nvk_image *img)
{
    nvk_cmdbuf c; if (nvmtl_vk_cmd_begin(q, &c)) return -1;
    if (nvmtl_vk_cmd_image_gen_mipmaps(&c, img)) { nvmtl_vk_cmd_abandon(&c); return -1; }
    return nvmtl_vk_submit_wait(&c);
}

int nvmtl_vk_image_view_create(nvk_image *img, int bgra, uint32_t baseLevel, uint32_t levelCount, void **out_view)
{
    return nvmtl_vk_image_view_create_ex(img, bgra ? VK_FORMAT_B8G8R8A8_UNORM : VK_FORMAT_R8G8B8A8_UNORM, 0, baseLevel, levelCount, out_view);
}
static VkImageViewType nvmtl_vk_view_type_of(const nvk_image *img) {
    switch (img->mtl_type) { case 0: return VK_IMAGE_VIEW_TYPE_1D; case 1: return VK_IMAGE_VIEW_TYPE_1D_ARRAY; case 3: return VK_IMAGE_VIEW_TYPE_2D_ARRAY;
        case 5: return VK_IMAGE_VIEW_TYPE_CUBE; case 6: return VK_IMAGE_VIEW_TYPE_CUBE_ARRAY; case 7: return VK_IMAGE_VIEW_TYPE_3D; default: return VK_IMAGE_VIEW_TYPE_2D; }
}
static uint32_t nvmtl_vk_view_layers_of(const nvk_image *img) { return img->mtl_type == 7 ? 1u : (img->layers ? img->layers : 1u); }
int nvmtl_vk_image_view_create_ex(nvk_image *img, uint32_t vkfmt, int a8, uint32_t baseLevel, uint32_t levelCount, void **out_view)
{
    if (nvmtl_vk_init()) return -1;
    if (!levelCount) levelCount = (img->mips ? img->mips : 1) - baseLevel;
    if (baseLevel + levelCount > (img->mips ? img->mips : 1)) {
        nvlog("image_view_create: levels [%u,%u) is outside the %u level(s) this texture has", baseLevel, baseLevel + levelCount, img->mips);
        return -1;
    }
    VkFormat fmt = (VkFormat)vkfmt;
    VkComponentMapping swz = a8 ? (VkComponentMapping){ VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_R }
                                : (VkComponentMapping){ 0, 0, 0, 0 };
    VkImageViewCreateInfo vci = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, (VkImage)img->img,
        nvmtl_vk_view_type_of(img), fmt, swz, { VK_IMAGE_ASPECT_COLOR_BIT, baseLevel, levelCount, 0, nvmtl_vk_view_layers_of(img) } };
    VkImageViewUsageCreateInfo uci;
    int vg = nvmtl_vk_view_guard(img, vkfmt, a8, vci.viewType, vci.subresourceRange.levelCount, &uci, "image_view_create");
    if (vg < 0) return -1;
    if (vg > 0) vci.pNext = &uci;
    VkImageView v = VK_NULL_HANDLE;
    VKCK(pvkCreateImageView(g_dev, &vci, NULL, &v), "vkCreateImageView(view)");
    *out_view = v;
    return 0;
}
int nvmtl_vk_image_view_create_aspect(nvk_image *img, uint32_t vkfmt, uint32_t aspect, uint32_t baseLevel, uint32_t levelCount, void **out_view)
{
    if (nvmtl_vk_init()) return -1;
    if (!levelCount) levelCount = (img->mips ? img->mips : 1) - baseLevel;
    if (baseLevel + levelCount > (img->mips ? img->mips : 1)) { nvlog("image_view_create(aspect): levels [%u,%u) is outside the %u level(s)", baseLevel, baseLevel + levelCount, img->mips); return -1; }
    if (img->fmt != vkfmt) { nvlog("image_view_create(aspect): view format %u is not the image's format %u", vkfmt, img->fmt); return -1; }
    VkImageViewCreateInfo vci = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, (VkImage)img->img,
        nvmtl_vk_view_type_of(img), (VkFormat)vkfmt, { 0 }, { (VkImageAspectFlags)aspect, baseLevel, levelCount, 0, nvmtl_vk_view_layers_of(img) } };
    VkImageView v = VK_NULL_HANDLE;
    VKCK(pvkCreateImageView(g_dev, &vci, NULL, &v), "vkCreateImageView(aspect view)");
    *out_view = v;
    return 0;
}
void nvmtl_vk_image_view_destroy(void *view) { if (view) pvkDestroyImageView(g_dev, (VkImageView)view, NULL); }
int nvmtl_vk_texel_on(void) { return g_mutable && pvkCreateBufferView != NULL; }
int nvmtl_vk_texel_view_create(nvk_buffer *b, size_t offset, size_t range, uint32_t vkfmt, int storage, void **alias, void **view, int *storage_ok)
{
    *alias = *view = NULL; if (storage_ok) *storage_ok = 0;
    if (!nvmtl_vk_texel_on()) { nvlog("texel buffer: not carried on this device (mutable descriptors %d, vkCreateBufferView %s)", g_mutable, pvkCreateBufferView ? "yes" : "NO"); return -1; }
    if (!b || !b->buf || !b->mem) { nvlog("texel buffer: the buffer has no memory of its own (pooled/imported) — not carried"); return -1; }
    if (!range || offset + range > b->size) { nvlog("texel buffer: [%zu,+%zu) is outside the %zu-byte buffer", offset, range, b->size); return -1; }
    static PFN_vkGetPhysicalDeviceFormatProperties gfp; if (!gfp) gfp = (PFN_vkGetPhysicalDeviceFormatProperties)pvkGetInstanceProcAddr(g_inst, "vkGetPhysicalDeviceFormatProperties");
    VkFormatProperties fp = { 0 }; if (gfp) gfp(g_pdev, (VkFormat)vkfmt, &fp);
    if (!(fp.bufferFeatures & VK_FORMAT_FEATURE_UNIFORM_TEXEL_BUFFER_BIT)) { nvlog("texel buffer: VkFormat %u is not a uniform texel format on this device", vkfmt); return -1; }
    const int st = storage && (fp.bufferFeatures & VK_FORMAT_FEATURE_STORAGE_TEXEL_BUFFER_BIT);
    if (storage && !st) { static int said; if (said++ < 4) nvlog("texel buffer: VkFormat %u is not a STORAGE texel format — the texture reads, its writes are refused", vkfmt); }
    VkBufferCreateInfo bci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, 0, range,
        VK_BUFFER_USAGE_UNIFORM_TEXEL_BUFFER_BIT | (st ? VK_BUFFER_USAGE_STORAGE_TEXEL_BUFFER_BIT : 0), VK_SHARING_MODE_EXCLUSIVE, 0, NULL };
    VkBuffer ab = VK_NULL_HANDLE;
    VKCK(pvkCreateBuffer(g_dev, &bci, NULL, &ab), "vkCreateBuffer(texel alias)");
    VkMemoryRequirements mr; pvkGetBufferMemoryRequirements(g_dev, ab, &mr);
    const VkDeviceSize at = (VkDeviceSize)b->moff + offset;
    if (mr.alignment && at % mr.alignment) {
        nvlog("texel buffer: offset %llu is not a multiple of the texel alignment %llu — refused", (unsigned long long)at, (unsigned long long)mr.alignment);
        pvkDestroyBuffer(g_dev, ab, NULL); return -1; }
    VkResult r = pvkBindBufferMemory(g_dev, ab, (VkDeviceMemory)b->mem, at);
    if (r != VK_SUCCESS) { nvlog("texel buffer: vkBindBufferMemory(alias) -> %d", r); pvkDestroyBuffer(g_dev, ab, NULL); return -1; }
    VkBufferViewCreateInfo vci = { VK_STRUCTURE_TYPE_BUFFER_VIEW_CREATE_INFO, NULL, 0, ab, (VkFormat)vkfmt, 0, range };
    VkBufferView v = VK_NULL_HANDLE;
    r = pvkCreateBufferView(g_dev, &vci, NULL, &v);
    if (r != VK_SUCCESS) { nvlog("texel buffer: vkCreateBufferView -> %d", r); pvkDestroyBuffer(g_dev, ab, NULL); return -1; }
    *alias = ab; *view = v; if (storage_ok) *storage_ok = st;
    return 0;
}
void nvmtl_vk_texel_view_destroy(void *alias, void *view)
{
    if (view && pvkDestroyBufferView) pvkDestroyBufferView(g_dev, (VkBufferView)view, NULL);
    if (alias) pvkDestroyBuffer(g_dev, (VkBuffer)alias, NULL);
}
int nvmtl_vk_bind_texel_view(nvk_cmdbuf *c, uint32_t set, uint32_t index, void *view, int storage)
{
    if (index >= NVMTL_NTEX) { nvlog("bind texel view: index %u is outside [0,%d)", index, NVMTL_NTEX); return -1; }
    int rc = nvmtl_bind_slot(c, set, NVMTL_NBUF + index, NVMTL_SLOT_TEXBUF, view, 0);
    if (index < NVMTL_NSTEX) nvmtl_bind_slot(c, set, NVMTL_STEX_BASE + index, storage && view ? NVMTL_SLOT_TEXBUF : NVMTL_SLOT_NONE, storage ? view : NULL, 0);
    return rc;
}
static void nvmtl_spirv_texel_mask(const void *p, size_t n, uint64_t out[4])
{
    memset(out, 0, 4 * sizeof *out);
    const uint32_t *w = (const uint32_t *)p; size_t nw = n / 4;
    if (!p || nw < 5 || w[0] != 0x07230203u || !w[3] || w[3] > (1u << 22)) return;
    const uint32_t bound = w[3];
    uint8_t *kind = (uint8_t *)calloc(bound, 1);
    if (!kind) return;
    for (int pass = 0; pass < 2; pass++)
        for (size_t i = 5; i < nw; ) {
            uint32_t wc = w[i] >> 16, op = w[i] & 0xffffu;
            if (!wc || i + wc > nw) break;
            if (pass == 0 && op == 25 && wc >= 4 && w[i + 1] < bound && w[i + 3] == 5) kind[w[i + 1]] = 1;
            if (pass == 0 && op == 32 && wc == 4 && w[i + 1] < bound && w[i + 3] < bound && kind[w[i + 3]] == 1) kind[w[i + 1]] = 2;
            if (pass == 0 && op == 59 && wc >= 4 && w[i + 2] < bound && w[i + 1] < bound && kind[w[i + 1]] == 2) kind[w[i + 2]] = 3;
            if (pass == 1 && op == 71 && wc == 4 && w[i + 2] == 33 && w[i + 1] < bound && kind[w[i + 1]] == 3) {
                uint32_t b = w[i + 3];
                if (b >= NVMTL_NBUF && b < NVMTL_NBUF + NVMTL_NTEX) { uint32_t k = b - NVMTL_NBUF; out[k >> 6] |= 1ull << (k & 63); }
                else if (b >= NVMTL_STEX_BINDING_NUMBER && b < NVMTL_STEX_BINDING_NUMBER + NVMTL_NSTEX) { uint32_t k = b - NVMTL_STEX_BINDING_NUMBER; out[2 + (k >> 6)] |= 1ull << (k & 63); }
            }
            i += wc;
        }
    free(kind);
}

int nvmtl_vk_bind_texture_view(nvk_cmdbuf *c, uint32_t set, uint32_t index, void *view)
{
    if (index >= NVMTL_NTEX) { nvlog("bind texture view: index %u is outside [0,%d)", index, NVMTL_NTEX); return -1; }
    const nvmtl_slot *was = (c->table && set < 2) ? &((nvmtl_bindtable *)c->table)->s[set][NVMTL_NBUF + index] : NULL;
    const int same = was && was->kind == NVMTL_SLOT_TEX && was->h == view;
    int r = nvmtl_bind_slot(c, set, NVMTL_NBUF + index, NVMTL_SLOT_TEX, view, 0);
    if (!r && !same && c->table && set < 2) { extern uint32_t nvmtl_bl_slot(void *); ((nvmtl_bindtable *)c->table)->s[set][NVMTL_NBUF + index].size = view ? nvmtl_bl_slot(view) : 0; }
    return r;
}
int nvmtl_vk_bind_storage_view(nvk_cmdbuf *c, uint32_t set, uint32_t index, void *view)
{
    if (index >= NVMTL_NSTEX) return -1;
    return nvmtl_bind_slot(c, set, NVMTL_STEX_BASE + index, view ? NVMTL_SLOT_TEX : NVMTL_SLOT_NONE, view, 0);
}

int nvmtl_vk_have_polygon_mode(void) { return g_dyn3; }
int nvmtl_vk_cmd_set_polygon_mode(nvk_cmdbuf *c, int line)
{
    if (!g_dyn3 || !pvkCmdSetPolygonModeEXT) return -1;
    pvkCmdSetPolygonModeEXT(c->cb, line ? VK_POLYGON_MODE_LINE : VK_POLYGON_MODE_FILL);
    return 0;
}

#include "nvmtl_visibility_sum_spv.h"
static pthread_mutex_t g_visibility_sum_lock = PTHREAD_MUTEX_INITIALIZER;
static VkPipeline g_visibility_sum_pipeline;
static VkPipelineLayout g_visibility_sum_layout;

static int nvmtl_visibility_sum_init(void)
{
    int rc = -1;
    pthread_mutex_lock(&g_visibility_sum_lock);
    if (g_visibility_sum_pipeline) { rc = 0; goto done; }
    VkShaderModule mod = VK_NULL_HANDLE;
    VkShaderModuleCreateInfo sm = { VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, NULL, 0,
                                   sizeof nvmtl_visibility_sum_spv, nvmtl_visibility_sum_spv };
    if (pvkCreateShaderModule(g_dev, &sm, NULL, &mod) != VK_SUCCESS) goto done;
    VkPushConstantRange push = { VK_SHADER_STAGE_COMPUTE_BIT, 0, 24 };
    VkPipelineLayoutCreateInfo li = { VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, NULL, 0, 0, NULL, 1, &push };
    VkPipelineLayout layout = VK_NULL_HANDLE;
    if (pvkCreatePipelineLayout(g_dev, &li, NULL, &layout) == VK_SUCCESS) {
        VkPipelineShaderStageCreateInfo stage = { VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0,
                                                 VK_SHADER_STAGE_COMPUTE_BIT, mod, "main", NULL };
        VkComputePipelineCreateInfo pi = { VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO, NULL, 0,
                                          stage, layout, VK_NULL_HANDLE, -1 };
        VkPipeline pipeline = VK_NULL_HANDLE;
        if (pvkCreateComputePipelines(g_dev, VK_NULL_HANDLE, 1, &pi, NULL, &pipeline) == VK_SUCCESS) {
            g_visibility_sum_layout = layout; g_visibility_sum_pipeline = pipeline; rc = 0;
        } else {
            if (pipeline) pvkDestroyPipeline(g_dev, pipeline, NULL);
            pvkDestroyPipelineLayout(g_dev, layout, NULL);
        }
    }
    pvkDestroyShaderModule(g_dev, mod, NULL);
done:
    pthread_mutex_unlock(&g_visibility_sum_lock);
    return rc;
}

int nvmtl_vk_cmd_sum_occlusion(nvk_cmdbuf *c, nvk_buffer *src, size_t first, uint32_t count, nvk_buffer *dst, size_t offset)
{
    if (!c || c->in_rp || !src || !dst || !count || first > src->size / 8 ||
        count > src->size / 8 - first || (offset & 7) || offset > dst->size || dst->size - offset < 8 ||
        nvmtl_visibility_sum_init()) return -1;
    struct { uint64_t src, dst; uint32_t count, pad; } args = {
        nvmtl_vk_buffer_address(src) + first * 8,
        nvmtl_vk_buffer_address(dst) + offset, count, 0
    };
    if (!args.src || !args.dst) return -1;
    nvmtl_vk_cmd_barrier(c);
    pvkCmdBindPipeline(c->cb, VK_PIPELINE_BIND_POINT_COMPUTE, g_visibility_sum_pipeline);
    pvkCmdPushConstants(c->cb, g_visibility_sum_layout, VK_SHADER_STAGE_COMPUTE_BIT, 0, sizeof args, &args);
    pvkCmdDispatch(c->cb, 1, 1, 1);
    nvmtl_vk_cmd_barrier(c);
    return 0;
}

enum { NVMTL_OCCLUSION_PAGE = 128 };
typedef struct nvmtl_occlusion_page {
    struct nvmtl_occlusion_page *next;
    VkQueryPool pool;
    uint32_t base;
} nvmtl_occlusion_page;

static nvmtl_occlusion_page *nvmtl_occlusion_find(nvk_cmdbuf *c, uint32_t query)
{
    for (nvmtl_occlusion_page *p = c->occlusion_pages; p; p = p->next)
        if (query >= p->base && query - p->base < NVMTL_OCCLUSION_PAGE) return p;
    return NULL;
}

int nvmtl_vk_cmd_alloc_occlusion(nvk_cmdbuf *c, uint32_t *query)
{
    if (!c || !c->cb || !query || !g_host_query_reset || !g_occlusion_precise ||
        !pvkResetQueryPool || c->occlusion_count == UINT32_MAX) {
        nvlog("visibility: precise host-reset queries unavailable"); return -1;
    }
    const uint32_t idx = c->occlusion_count;
    nvmtl_occlusion_page *p = nvmtl_occlusion_find(c, idx);
    if (!p) {
        p = calloc(1, sizeof *p);
        if (!p) return -1;
        VkQueryPoolCreateInfo qi = { VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO, NULL, 0,
                                    VK_QUERY_TYPE_OCCLUSION, NVMTL_OCCLUSION_PAGE, 0 };
        if (pvkCreateQueryPool(g_dev, &qi, NULL, &p->pool) != VK_SUCCESS) { free(p); return -1; }
        pvkResetQueryPool(g_dev, p->pool, 0, NVMTL_OCCLUSION_PAGE);
        p->base = idx; p->next = c->occlusion_pages; c->occlusion_pages = p;
    }
    *query = idx; c->occlusion_count++;
    return 0;
}

void nvmtl_vk_cmd_retire_occlusion(nvk_cmdbuf *c)
{
    nvmtl_occlusion_page *p = c->occlusion_pages;
    while (p) {
        nvmtl_occlusion_page *next = p->next;
        pvkDestroyQueryPool(g_dev, p->pool, NULL); free(p); p = next;
    }
    c->occlusion_pages = NULL; c->occlusion_count = 0;
}

int nvmtl_vk_cmd_begin_occlusion(nvk_cmdbuf *c, uint32_t idx)
{
    nvmtl_occlusion_page *p = nvmtl_occlusion_find(c, idx);
    if (!p || !c->in_rp) return -1;
    pvkCmdBeginQuery(c->cb, p->pool, idx - p->base, VK_QUERY_CONTROL_PRECISE_BIT);
    return 0;
}

void nvmtl_vk_cmd_end_occlusion(nvk_cmdbuf *c, uint32_t idx)
{
    nvmtl_occlusion_page *p = nvmtl_occlusion_find(c, idx);
    if (p && c->in_rp) pvkCmdEndQuery(c->cb, p->pool, idx - p->base);
}

int nvmtl_vk_cmd_copy_occlusion(nvk_cmdbuf *c, uint32_t first, uint32_t count, nvk_buffer *dst, size_t off)
{
    nvmtl_occlusion_page *p = nvmtl_occlusion_find(c, first);
    if (!p || !count || c->in_rp || first - p->base + count > NVMTL_OCCLUSION_PAGE ||
        !dst || !dst->buf || off > dst->size || (size_t)count * 8 > dst->size - off) return -1;
    pvkCmdCopyQueryPoolResults(c->cb, p->pool, first - p->base, count, (VkBuffer)dst->buf, off, 8,
                              VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WAIT_BIT);
    return 0;
}

int nvmtl_vk_cmd_set_depth_clip(nvk_cmdbuf *c, int clip)
{
    if (!g_clip || !pvkCmdSetDepthClipEnableEXT) return -1;
    pvkCmdSetDepthClipEnableEXT(c->cb, clip ? VK_TRUE : VK_FALSE);
    return 0;
}

#define NVMTL_COUNTER_PAGE 64u
typedef struct nvmtl_counter_state { uint32_t count; nvk_buffer values; } nvmtl_counter_state;
typedef struct nvmtl_counter_page { struct nvmtl_counter_page *next; VkQueryPool pool; uint32_t used; } nvmtl_counter_page;
typedef struct { nvmtl_counter_state *state; VkQueryPool pool; uint32_t query, index; } nvmtl_counter_write;
typedef struct {
    nvk_cmdbuf reset;
    nvmtl_counter_page *pages;
    nvmtl_counter_write *pending;
    size_t count, capacity;
    unsigned reset_ended;
} nvmtl_counter_recording;

static int nvmtl_counter_fail(nvk_cmdbuf *c) { if (c) c->counter_error = 1; return -1; }
static void nvmtl_counter_memory_barrier(nvk_cmdbuf *c)
{
    VkMemoryBarrier mb = { VK_STRUCTURE_TYPE_MEMORY_BARRIER, NULL,
        VK_ACCESS_TRANSFER_WRITE_BIT | VK_ACCESS_HOST_WRITE_BIT,
        VK_ACCESS_TRANSFER_READ_BIT | VK_ACCESS_TRANSFER_WRITE_BIT | VK_ACCESS_HOST_READ_BIT };
    pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_TRANSFER_BIT | VK_PIPELINE_STAGE_HOST_BIT,
        VK_PIPELINE_STAGE_TRANSFER_BIT | VK_PIPELINE_STAGE_HOST_BIT, 0, 1, &mb, 0, NULL, 0, NULL);
}
static int nvmtl_counter_flush(nvk_cmdbuf *c)
{
    if (!c || c->counter_error) return -1;
    nvmtl_counter_recording *r = c->counter_recording;
    if (!r || !r->count) return 0;
    if (c->in_rp) return nvmtl_counter_fail(c);
    for (size_t i = 0; i < r->count; ++i) {
        nvmtl_counter_write *w = &r->pending[i];
        nvmtl_counter_memory_barrier(c);
        pvkCmdCopyQueryPoolResults(c->cb, w->pool, w->query, 1,
            (VkBuffer)w->state->values.buf, (VkDeviceSize)w->index * 8, 8,
            VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WAIT_BIT);
    }
    r->count = 0;
    nvmtl_counter_memory_barrier(c);
    return 0;
}
static int nvmtl_counter_reset_finish(nvk_cmdbuf *c, VkCommandBuffer *reset)
{
    *reset = VK_NULL_HANDLE;
    nvmtl_counter_recording *r = c->counter_recording;
    if (!r) return 0;
    if (r->reset_ended || !r->reset.cb || r->reset.in_rp) return nvmtl_counter_fail(c);
    if (pvkEndCommandBuffer(r->reset.cb) != VK_SUCCESS) return nvmtl_counter_fail(c);
    r->reset_ended = 1; *reset = (VkCommandBuffer)r->reset.cb;
    return 0;
}
static void nvmtl_counter_retire(nvk_cmdbuf *c)
{
    nvmtl_counter_recording *r = c->counter_recording;
    if (!r) return;
    r->reset.wseq = c->wseq;
    nvmtl_cmd_retire(&r->reset);
    for (nvmtl_counter_page *p = r->pages; p;) {
        nvmtl_counter_page *next = p->next;
        pvkDestroyQueryPool(g_dev, p->pool, NULL); free(p); p = next;
    }
    free(r->pending); free(r); c->counter_recording = NULL;
}
void *nvmtl_vk_ts_pool_create(uint32_t count)
{
    if (!count || (size_t)count > SIZE_MAX / 8 || nvmtl_vk_init() || !g_timestamp_bits || !pvkCreateQueryPool || !pvkCmdWriteTimestamp || !pvkCmdResetQueryPool || !pvkCmdCopyQueryPoolResults) return NULL;
    nvmtl_counter_state *s = calloc(1, sizeof *s);
    if (!s) return NULL;
    s->count = count;
    if (nvmtl_vk_buffer_create((size_t)count * 8, 1, &s->values) || !s->values.map) {
        nvmtl_vk_buffer_destroy(&s->values); free(s); return NULL;
    }
    memset(s->values.map, 0, (size_t)count * 8);
    return s;
}
void nvmtl_vk_ts_pool_destroy(void *pool)
{
    nvmtl_counter_state *s = pool;
    if (!s) return;
    if (g_dev && g_state == 1) nvmtl_vk_buffer_destroy(&s->values);
    free(s);
}
int nvmtl_vk_cmd_ts_write(nvk_cmdbuf *c, void *pool, uint32_t idx, int barrier)
{
    nvmtl_counter_state *s = pool;
    if (!c || !c->cb || !s || idx >= s->count || c->counter_error) return nvmtl_counter_fail(c);
    nvmtl_counter_recording *r = c->counter_recording;
    if (!r) {
        r = calloc(1, sizeof *r);
        if (!r) return nvmtl_counter_fail(c);
        c->counter_recording = r;
        if (nvmtl_vk_cmd_begin(NULL, &r->reset)) return nvmtl_counter_fail(c);
    }
    if (r->reset_ended) return nvmtl_counter_fail(c);
    if (r->count == r->capacity) {
        size_t cap = r->capacity ? r->capacity * 2 : 16;
        if (cap < r->capacity || cap > SIZE_MAX / sizeof *r->pending) return nvmtl_counter_fail(c);
        void *q = realloc(r->pending, cap * sizeof *r->pending);
        if (!q) return nvmtl_counter_fail(c);
        r->pending = q; r->capacity = cap;
    }
    nvmtl_counter_page *p = r->pages;
    if (!p || p->used == NVMTL_COUNTER_PAGE) {
        p = calloc(1, sizeof *p);
        if (!p) return nvmtl_counter_fail(c);
        VkQueryPoolCreateInfo qi = { VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO, NULL, 0,
            VK_QUERY_TYPE_TIMESTAMP, NVMTL_COUNTER_PAGE, 0 };
        if (pvkCreateQueryPool(g_dev, &qi, NULL, &p->pool) != VK_SUCCESS) { free(p); return nvmtl_counter_fail(c); }
        p->next = r->pages; r->pages = p;
        pvkCmdResetQueryPool(r->reset.cb, p->pool, 0, NVMTL_COUNTER_PAGE);
    }
    uint32_t query = p->used++;
    pvkCmdWriteTimestamp(c->cb, barrier ? VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT : VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
        p->pool, query);
    r->pending[r->count++] = (nvmtl_counter_write){s, p->pool, query, idx};
    return c->in_rp ? 0 : nvmtl_counter_flush(c);
}
int nvmtl_vk_cmd_ts_resolve(nvk_cmdbuf *c, void *pool, uint32_t first, uint32_t count, nvk_buffer *dst, size_t off)
{
    nvmtl_counter_state *s = pool;
    if (!c || !c->cb || !s || !count || first > s->count || count > s->count-first ||
        !dst || !dst->buf || (off & 7) || off > dst->size || (size_t)count*8 > dst->size-off || c->in_rp)
        return nvmtl_counter_fail(c);
    if (nvmtl_counter_flush(c)) return -1;
    nvmtl_counter_memory_barrier(c);
    if (nvmtl_vk_cmd_copy_buffer(c, &s->values, (size_t)first*8, dst, off, (size_t)count*8)) return nvmtl_counter_fail(c);
    nvmtl_vk_cmd_barrier(c);
    return 0;
}
int nvmtl_vk_ts_read(void *pool, uint32_t first, uint32_t count, uint64_t *out)
{
    nvmtl_counter_state *s = pool;
    if (!s || !out || !count || first > s->count || count > s->count-first || !s->values.map || g_state != 1) return -1;
    memcpy(out, (const char *)s->values.map + (size_t)first*8, (size_t)count*8);
    return 0;
}
static pthread_mutex_t g_cal_lock = PTHREAD_MUTEX_INITIALIZER;
static VkQueryPool g_cal_pool;
static int64_t g_cal_off;
static uint64_t g_cal_at;
int nvmtl_vk_gpu_timestamp(uint64_t *cpu_ns, uint64_t *gpu)
{
    pthread_mutex_lock(&g_cal_lock);
    uint64_t now = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
    if (!g_cal_at || now - g_cal_at > 1000000000ull) {
        if (!g_cal_pool && !nvmtl_vk_init() && g_timestamp_bits && pvkCreateQueryPool) {
            VkQueryPoolCreateInfo qi = { VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO, NULL, 0,
                VK_QUERY_TYPE_TIMESTAMP, 1, 0 };
            VkQueryPool pool = VK_NULL_HANDLE;
            if (pvkCreateQueryPool(g_dev, &qi, NULL, &pool) == VK_SUCCESS) g_cal_pool = pool;
        }
        uint64_t best = UINT64_MAX; int64_t off = 0;
        for (int k = 0; g_cal_pool && k < 3; k++) {
            nvk_cmdbuf cb; if (nvmtl_vk_cmd_begin(NULL, &cb)) break;
            pvkCmdResetQueryPool(cb.cb, g_cal_pool, 0, 1);
            pvkCmdWriteTimestamp(cb.cb, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, g_cal_pool, 0);
            const uint64_t t0 = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
            const int rc = nvmtl_vk_submit_wait(&cb);
            const uint64_t t1 = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
            if (rc) break;
            uint64_t result[2] = {0};
            VkResult qr = pvkGetQueryPoolResults(g_dev, g_cal_pool, 0, 1,
                sizeof result, result, 16, VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WITH_AVAILABILITY_BIT);
            const uint64_t g = result[0];
            if (qr != VK_SUCCESS || !result[1] || !g) break;
            if (t1 - t0 < best) { best = t1 - t0; off = (int64_t)(g - (t0 + (t1 - t0) / 2)); }
        }
        if (best != UINT64_MAX) {
            if (!g_cal_at || llabs(off - g_cal_off) > 1000000)
                nvlog("counters: GPU clock %s - gpu - cpu = %lld ns, bracket %llu ns (tightest of 3)",
                      g_cal_at ? "RE-CALIBRATED, offset moved" : "calibrated", (long long)off, (unsigned long long)best);
            g_cal_off = off; g_cal_at = now;
        }
    }
    const int ok = g_cal_at != 0;
    now = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
    if (cpu_ns) *cpu_ns = now;
    if (gpu) *gpu = ok ? (uint64_t)((int64_t)now + g_cal_off) : 0;
    pthread_mutex_unlock(&g_cal_lock);
    return ok ? 0 : -1;
}

uint64_t nvmtl_vk_buffer_address(nvk_buffer *b)
{
    if (!b || !b->buf || !pvkGetBufferDeviceAddress) return 0;
    VkBufferDeviceAddressInfo i = { VK_STRUCTURE_TYPE_BUFFER_DEVICE_ADDRESS_INFO, NULL, (VkBuffer)b->buf };
    return pvkGetBufferDeviceAddress(g_dev, &i);
}

int nvmtl_vk_cmd_fill_buffer(nvk_cmdbuf *c, nvk_buffer *b, size_t offset, size_t size, uint8_t value)
{
    if (!size) return 0;
    if ((offset | size) & 3 || offset > b->size || size > b->size - offset) return -1;
    nvmtl_vk_cmd_barrier(c);
    uint32_t word = value; word |= word << 8; word |= word << 16;
    pvkCmdFillBuffer(c->cb, (VkBuffer)b->buf, offset, size, word);
    nvmtl_vk_cmd_barrier(c);
    return 0;
}

int nvmtl_vk_cmd_write_word(nvk_cmdbuf *c, nvk_buffer *b, size_t offset, uint32_t word)
{
    if ((offset & 3) || offset > b->size || b->size - offset < 4) return -1;
    nvmtl_vk_cmd_barrier(c);
    pvkCmdFillBuffer(c->cb, (VkBuffer)b->buf, offset, 4, word);
    nvmtl_vk_cmd_barrier(c);
    return 0;
}

int nvmtl_vk_cmd_copy_buffer(nvk_cmdbuf *c, nvk_buffer *src, size_t srcOff, nvk_buffer *dst, size_t dstOff, size_t size)
{
    if (!size) return 0;
    if (srcOff > src->size || size > src->size-srcOff || dstOff > dst->size || size > dst->size-dstOff) return -1;
    nvmtl_vk_cmd_barrier(c);
    VkBufferCopy bc = { srcOff, dstOff, size };
    pvkCmdCopyBuffer(c->cb, (VkBuffer)src->buf, (VkBuffer)dst->buf, 1, &bc);
    nvmtl_vk_cmd_barrier(c);
    return 0;
}

int nvmtl_vk_cmd_copy_image(nvk_cmdbuf *c, nvk_image *src, uint32_t sx, uint32_t sy,
                        nvk_image *dst, uint32_t dx, uint32_t dy, uint32_t w, uint32_t h)
{
    return nvmtl_vk_cmd_copy_image_sub(c, src, 0, 0, sx, sy, dst, 0, 0, dx, dy, w, h);
}

static inline uint32_t nvmtl_lvl(uint32_t d, uint32_t l) { d >>= l; return d ? d : 1; }
int nvmtl_vk_cmd_copy_buffer_to_image_layer(nvk_cmdbuf *c, nvk_buffer *b, size_t off, uint32_t row_bytes,
                                  nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t layer)
{ return nvmtl_vk_cmd_copy_buffer_to_image_level_layer(c, b, off, row_bytes, img, x, y, w, h, 0, layer); }
int nvmtl_vk_cmd_copy_buffer_to_image_level_layer(nvk_cmdbuf *c, nvk_buffer *b, size_t off, uint32_t row_bytes,
                                  nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t level, uint32_t layer)
{
    if (level >= (img->mips ? img->mips : 1)) { nvlog("copy_buffer_to_image: level %u — this image has %u", level, img->mips ? img->mips : 1); return -1; }
    const uint32_t lw = nvmtl_lvl(img->w, level), lh = nvmtl_lvl(img->h, level);
    const uint32_t bpp = g_copy_bpp_override ? g_copy_bpp_override : (img->bpp ? img->bpp : 4);
    const uint32_t bw = img->bw ? img->bw : 1, bh = img->bh ? img->bh : 1;
    const uint32_t cols = nvmtl_blocks(w, bw), rows = nvmtl_blocks(h, bh);
    const uint64_t tight = (uint64_t)cols * bpp;
    if (!row_bytes) { if (tight > UINT32_MAX) return -1; row_bytes = (uint32_t)tight; }
    if (bw > 1 && ((x % bw) || (y % bh))) { nvlog("copy: origin (%u,%u) is not block-aligned", x, y); return -1; }
    if (layer >= (img->layers ? img->layers : 1) || !w || !h || x > lw || w > lw-x || y > lh || h > lh-y
        || row_bytes < tight || row_bytes % bpp || off > b->size || (uint64_t)(rows-1)*row_bytes+tight > b->size-off) return -1;
    if (!w || !h || x + w > lw || y + h > lh) { nvlog("copy_buffer_to_image: out of bounds (level %u is %ux%u)", level, lw, lh); return -1; }
    nvmtl_vk_cmd_barrier(c);
    img_to(c, img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_ACCESS_MEMORY_WRITE_BIT | VK_ACCESS_MEMORY_READ_BIT, VK_ACCESS_TRANSFER_WRITE_BIT,
           VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
    VkBufferImageCopy bic = { off, row_bytes / bpp * bw, 0, { nvmtl_copy_aspect(img), level, img->mtl_type == 7 ? 0u : layer, 1 },
                              { (int32_t)x, (int32_t)y, img->mtl_type == 7 ? (int32_t)layer : 0 }, { w, h, 1 } };
    pvkCmdCopyBufferToImage(c->cb, (VkBuffer)b->buf, (VkImage)img->img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &bic);
    img_to(c, img, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_SHADER_READ_BIT,
           VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT);
    nvmtl_vk_cmd_barrier(c);
    return 0;
}

int nvmtl_vk_cmd_copy_image_to_buffer_layer(nvk_cmdbuf *c, nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h,
                                  nvk_buffer *b, size_t off, uint32_t row_bytes, uint32_t layer)
{ return nvmtl_vk_cmd_copy_image_to_buffer_level_layer(c, img, x, y, w, h, b, off, row_bytes, 0, layer); }
int nvmtl_vk_cmd_copy_image_to_buffer_level_layer(nvk_cmdbuf *c, nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h,
                                  nvk_buffer *b, size_t off, uint32_t row_bytes, uint32_t level, uint32_t layer)
{
    if (level >= (img->mips ? img->mips : 1)) { nvlog("copy_image_to_buffer: level %u — this image has %u", level, img->mips ? img->mips : 1); return -1; }
    const uint32_t lw = nvmtl_lvl(img->w, level), lh = nvmtl_lvl(img->h, level);
    const uint32_t bpp = g_copy_bpp_override ? g_copy_bpp_override : (img->bpp ? img->bpp : 4);
    const uint32_t bw = img->bw ? img->bw : 1, bh = img->bh ? img->bh : 1;
    const uint32_t cols = nvmtl_blocks(w, bw), rows = nvmtl_blocks(h, bh);
    const uint64_t tight = (uint64_t)cols * bpp;
    if (!row_bytes) { if (tight > UINT32_MAX) return -1; row_bytes = (uint32_t)tight; }
    if (bw > 1 && ((x % bw) || (y % bh))) { nvlog("copy: origin (%u,%u) is not block-aligned", x, y); return -1; }
    if (layer >= (img->layers ? img->layers : 1) || !w || !h || x > lw || w > lw-x || y > lh || h > lh-y
        || row_bytes < tight || row_bytes % bpp || off > b->size || (uint64_t)(rows-1)*row_bytes+tight > b->size-off) return -1;
    if (!w || !h || x + w > lw || y + h > lh) { nvlog("copy_image_to_buffer: out of bounds (level %u is %ux%u)", level, lw, lh); return -1; }
    nvmtl_vk_cmd_barrier(c);
    img_to(c, img, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_ACCESS_MEMORY_WRITE_BIT | VK_ACCESS_MEMORY_READ_BIT, VK_ACCESS_TRANSFER_READ_BIT,
           VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
    VkBufferImageCopy bic = { off, row_bytes / bpp * bw, 0, { nvmtl_copy_aspect(img), level, img->mtl_type == 7 ? 0u : layer, 1 },
                              { (int32_t)x, (int32_t)y, img->mtl_type == 7 ? (int32_t)layer : 0 }, { w, h, 1 } };
    pvkCmdCopyImageToBuffer(c->cb, (VkImage)img->img, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, (VkBuffer)b->buf, 1, &bic);
    nvmtl_vk_cmd_barrier(c);
    return 0;
}

int nvmtl_vk_cmd_image_gen_mipmaps(nvk_cmdbuf *c, nvk_image *img)
{
    if (!img->mips || img->mips < 2) { nvlog("generateMipmaps: texture has %u level(s); nothing to generate", img->mips); return 0; }
    nvmtl_vk_cmd_barrier(c);
    img_to(c, img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_ACCESS_MEMORY_WRITE_BIT | VK_ACCESS_MEMORY_READ_BIT, VK_ACCESS_TRANSFER_WRITE_BIT,
           VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
    int32_t mw = (int32_t)img->w, mh = (int32_t)img->h;
    for (uint32_t i = 1; i < img->mips; i++) {
        VkImageSubresourceRange src = { VK_IMAGE_ASPECT_COLOR_BIT, i - 1, 1, 0, 1 };
        VkImageMemoryBarrier toSrc = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL,
            VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_TRANSFER_READ_BIT,
            VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)img->img, src };
        pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &toSrc);
        int32_t nw = mw > 1 ? mw / 2 : 1, nh = mh > 1 ? mh / 2 : 1;
        VkImageBlit b = { { VK_IMAGE_ASPECT_COLOR_BIT, i - 1, 0, 1 }, { { 0, 0, 0 }, { mw, mh, 1 } },
                          { VK_IMAGE_ASPECT_COLOR_BIT, i,     0, 1 }, { { 0, 0, 0 }, { nw, nh, 1 } } };
        pvkCmdBlitImage(c->cb, (VkImage)img->img, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                        (VkImage)img->img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &b, VK_FILTER_LINEAR);
        mw = nw; mh = nh;
    }
    VkImageMemoryBarrier fin[2] = {
        { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, VK_ACCESS_TRANSFER_READ_BIT, VK_ACCESS_SHADER_READ_BIT,
          VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
          VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)img->img,
          { VK_IMAGE_ASPECT_COLOR_BIT, 0, img->mips - 1, 0, 1 } },
        { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_SHADER_READ_BIT,
          VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
          VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, (VkImage)img->img,
          { VK_IMAGE_ASPECT_COLOR_BIT, img->mips - 1, 1, 0, 1 } },
    };
    pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, 0, 0, NULL, 0, NULL, 2, fin);
    img->layout = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
    nvmtl_vk_cmd_barrier(c);
    return 0;
}

#define NVMTL_BUFFER_USAGE (VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT | VK_BUFFER_USAGE_VERTEX_BUFFER_BIT | \
        VK_BUFFER_USAGE_INDEX_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT | \
        VK_BUFFER_USAGE_INDIRECT_BUFFER_BIT | VK_BUFFER_USAGE_SHADER_DEVICE_ADDRESS_BIT | NVMTL_RT_BUFFER_USAGE)
static uint32_t heap_memtype_bits(void)
{
    static uint32_t bits; static int done;
    if (done) return bits;
    bits = ~0u;
    VkBufferCreateInfo bci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, 0, 4096, NVMTL_BUFFER_USAGE, VK_SHARING_MODE_EXCLUSIVE, 0, NULL };
    VkBuffer b = VK_NULL_HANDLE;
    if (pvkCreateBuffer(g_dev, &bci, NULL, &b) == VK_SUCCESS) { VkMemoryRequirements mr; pvkGetBufferMemoryRequirements(g_dev, b, &mr); bits &= mr.memoryTypeBits; pvkDestroyBuffer(g_dev, b, NULL); }
    VkImageCreateInfo ici = { VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, NULL, 0, VK_IMAGE_TYPE_2D, VK_FORMAT_R8G8B8A8_UNORM, { 64, 64, 1 }, 1, 1,
        VK_SAMPLE_COUNT_1_BIT, VK_IMAGE_TILING_OPTIMAL,
        VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_SAMPLED_BIT,
        VK_SHARING_MODE_EXCLUSIVE, 0, NULL, VK_IMAGE_LAYOUT_UNDEFINED };
    VkImage im = VK_NULL_HANDLE;
    if (nvmtl_typed_CreateImage(g_dev, &ici, NULL, &im) == VK_SUCCESS) { VkMemoryRequirements mr; pvkGetImageMemoryRequirements(g_dev, im, &mr); bits &= mr.memoryTypeBits; pvkDestroyImage(g_dev, im, NULL); }
    ici.format = VK_FORMAT_D32_SFLOAT_S8_UINT; ici.usage = NVMTL_DEPTH_USAGE; im = VK_NULL_HANDLE;
    if (nvmtl_typed_CreateImage(g_dev, &ici, NULL, &im) == VK_SUCCESS) { VkMemoryRequirements mr; pvkGetImageMemoryRequirements(g_dev, im, &mr); bits &= mr.memoryTypeBits; pvkDestroyImage(g_dev, im, NULL); }
    done = 1;
    nvlog("vk: placement-heap memory types: 0x%x", bits);
    return bits;
}
static int nvmtl_vk_heap_create_body(size_t size, int host_visible, nvk_heap *out);
int nvmtl_vk_heap_create(size_t size, int host_visible, nvk_heap *out)
{
    const uint64_t t0 = nvmtl_perf_now();
    nvk_heap pending = {0};
    const int r = nvmtl_vk_heap_create_body(size, host_visible, &pending);
    if (r) memset(out, 0, sizeof *out);
    else *out = pending;
    nvmtl_perf_note(NVP_HEAP, t0, size);
    return r;
}
static int nvmtl_vk_heap_create_body(size_t size, int host_visible, nvk_heap *out)
{
    if (nvmtl_vk_init()) return -1;
    memset(out, 0, sizeof *out); out->size = size; out->host_visible = host_visible;
    uint32_t bits = heap_memtype_bits();
    int mt = memtype(bits, host_visible ? (VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) : VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    if (mt < 0) { nvlog("vk: no memory type serves a %s placement heap (bits 0x%x)", host_visible ? "shared" : "private", bits); return -1; }
    VkMemoryAllocateFlagsInfo mafi = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO, NULL, VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT, 0 };
    VkMemoryAllocateInfo mai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &mafi, size, (uint32_t)mt };
    VkDeviceMemory mem = VK_NULL_HANDLE;
    { uint64_t hbud = 0, hused = 0, hfree = 0; int hmeas = 0;
      int hgate = host_visible ? 0 : nvmtl_vk_vram_wall(size, &hfree, &hbud, &hused, &hmeas);
      VkResult r_ = hgate ? VK_ERROR_OUT_OF_DEVICE_MEMORY : pvkAllocateMemory(g_dev, &mai, NULL, &mem);
      if (r_ == VK_SUCCESS && !host_visible) nvmtl_vk_budget_charge(size);
      if (r_ == VK_ERROR_OUT_OF_DEVICE_MEMORY && !host_visible) {
          int mt2 = -1; for (uint32_t i = 0; i < g_memp.memoryTypeCount; i++) if ((bits & (1u << i)) && !(g_memp.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) { mt2 = (int)i; break; }
          if (mt2 >= 0) { mai.memoryTypeIndex = (uint32_t)mt2; r_ = pvkAllocateMemory(g_dev, &mai, NULL, &mem); mt = mt2;
              if (r_ == VK_SUCCESS) { g_heap_vram_refused++;
                  nvlog("vk: private placement heap of %zu bytes REFUSED VRAM (%s) -> SYSTEM MEMORY: %llu MB free, "
                        "%llu MB headroom, heapUsage %llu of budget %llu MB (%s), %llu heaps refused so far",
                        size, hgate ? "our soft gate" : "RM refused it", (unsigned long long)(hfree >> 20),
                        (unsigned long long)(NVMTL_VRAM_HEADROOM >> 20), (unsigned long long)(hused >> 20),
                        (unsigned long long)(hbud >> 20), hmeas ? "measured" : "estimated",
                        (unsigned long long)g_heap_vram_refused); } }
      }
      if (r_ != VK_SUCCESS) { nvlog("vk: vkAllocateMemory(placement heap) -> %d", r_); return -1; } }
    out->mem = mem; out->memtype = (uint32_t)mt;
    if (host_visible) {
        VkResult mapped = pvkMapMemory(g_dev, mem, 0, VK_WHOLE_SIZE, 0, &out->map);
        if (mapped != VK_SUCCESS) { pvkFreeMemory(g_dev, mem, NULL); memset(out, 0, sizeof *out); return -1; }
    }
    g_alloc_bytes += size;
    return 0;
}
void nvmtl_vk_heap_destroy(nvk_heap *h)
{
    if (g_state != 1 || !h || !h->mem) return;
    if (h->map && pvkUnmapMemory) { pvkUnmapMemory(g_dev, (VkDeviceMemory)h->mem); h->map = NULL; }
    pvkFreeMemory(g_dev, (VkDeviceMemory)h->mem, NULL); g_alloc_bytes -= h->size;
    memset(h, 0, sizeof *h);
}
static int placed_ok(const VkMemoryRequirements *mr, const nvk_heap *heap, size_t offset, const char *what)
{
    if (!(mr->memoryTypeBits & (1u << heap->memtype))) { nvlog("vk: %s cannot live in memory type %u", what, heap->memtype); return -1; }
    if (mr->alignment && (offset % mr->alignment)) { nvlog("vk: %s offset %zu breaks alignment %llu", what, offset, (unsigned long long)mr->alignment); return -1; }
    if (offset + mr->size > heap->size) { nvlog("vk: %s at %zu + %llu overruns the heap (%zu)", what, offset, (unsigned long long)mr->size, heap->size); return -1; }
    return 0;
}
int nvmtl_vk_buffer_create_placed(size_t size, nvk_heap *heap, size_t offset, nvk_buffer *out)
{
    if (nvmtl_vk_init() || !heap || !heap->mem) return -1;
    memset(out, 0, sizeof *out); out->size = size;
    VkBufferCreateInfo bci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, 0, size, NVMTL_BUFFER_USAGE, VK_SHARING_MODE_EXCLUSIVE, 0, NULL };
    VKCK(nvmtl_opaque_CreateBuffer(g_dev, &bci, NULL, &out->buf), "vkCreateBuffer(placed)");
    VkMemoryRequirements mr; pvkGetBufferMemoryRequirements(g_dev, out->buf, &mr);
    if (placed_ok(&mr, heap, offset, "placed buffer")) { pvkDestroyBuffer(g_dev, (VkBuffer)out->buf, NULL); memset(out, 0, sizeof *out); return -1; }
    if (pvkBindBufferMemory(g_dev, out->buf, (VkDeviceMemory)heap->mem, offset) != VK_SUCCESS) {
        pvkDestroyBuffer(g_dev, (VkBuffer)out->buf, NULL); memset(out, 0, sizeof *out); return -1;
    }
    out->mem = heap->mem; out->placed = 1; out->alloc = 0;
    out->moff = offset;
    out->map = heap->map ? (char *)heap->map + offset : NULL;
    g_buf_made++;
    return 0;
}

static int nvmtl_vk_image_create_buffer_alias_lifetime_body(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, int storage, int render,
                                       const nvk_buffer *b, size_t offset, size_t row_pitch, nvk_image *out);
int nvmtl_vk_image_create_buffer_alias(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, int storage, int render,
                                       const nvk_buffer *b, size_t offset, size_t row_pitch, nvk_image *out)
{
    nvk_image pending = {0};
    int result = nvmtl_vk_image_create_buffer_alias_lifetime_body(w, h, vkfmt, bpp, a8, storage, render, b, offset, row_pitch, &pending);
    if (result) { nvmtl_failed_image(&pending); memset(out, 0, sizeof *out); }
    else { nvmtl_commit_image(&pending); *out = pending; }
    return result;
}
static int nvmtl_vk_image_create_buffer_alias_lifetime_body(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, int storage, int render,
                                       const nvk_buffer *b, size_t offset, size_t row_pitch, nvk_image *out)
{
    if (nvmtl_vk_init() || !b || !b->buf || !b->mem || !w || !h) { nvlog("buffer texture: no buffer memory to alias (%ux%u)", w, h); return -1; }
    { uint32_t bw = 0, bh = 0, bb = 0;
      if (nvmtl_vk_format_block(vkfmt, &bw, &bh, &bb)) { nvlog("buffer texture: VkFormat %u is block-compressed; a linear alias of it is not built", vkfmt); return -1; } }
    if (!bpp) bpp = 4;
    if (row_pitch < (size_t)w * bpp || row_pitch > UINT32_MAX || row_pitch % bpp) {
        nvlog("buffer texture: bytesPerRow %zu cannot hold %u texels of %u B", row_pitch, w, bpp); return -1; }
    if (g_linmod && row_pitch % 32) {
        nvlog("buffer texture: bytesPerRow %zu is not a multiple of 32 B (the texture header's linear pitch unit)", row_pitch); return -1; }
    if (offset > b->size || (size_t)row_pitch * (h - 1) + (size_t)w * bpp > b->size - offset) {
        nvlog("buffer texture: %ux%u at offset %zu pitch %zu overruns the %zu-byte buffer", w, h, offset, row_pitch, b->size); return -1; }
    PFN_vkGetPhysicalDeviceFormatProperties gfp = (PFN_vkGetPhysicalDeviceFormatProperties)pvkGetInstanceProcAddr(g_inst, "vkGetPhysicalDeviceFormatProperties");
    VkFormatProperties fp; memset(&fp, 0, sizeof fp);
    if (gfp) gfp(g_pdev, (VkFormat)vkfmt, &fp);
    VkFormatFeatureFlags lf = fp.linearTilingFeatures;
    if (!(lf & VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT)) { nvlog("buffer texture: VkFormat %u cannot be sampled LINEAR (features 0x%x)", vkfmt, (unsigned)lf); return -1; }
    int wantS = storage, wantR = render;
    storage = storage && !a8 && (lf & VK_FORMAT_FEATURE_STORAGE_IMAGE_BIT);
    render = render && (lf & VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT);
    if (wantS != storage || wantR != render)
        nvlog("buffer texture: VkFormat %u linear features 0x%x drop%s%s", vkfmt, (unsigned)lf, wantS != storage ? " ShaderWrite" : "", wantR != render ? " RenderTarget" : "");
    memset(out, 0, sizeof *out); out->w = w; out->h = h; out->mips = 1; out->fmt = vkfmt; out->bpp = bpp;
    out->storage = storage ? 1 : 0; out->mtl_type = 2; out->layers = 1;
    VkSubresourceLayout pl = { 0, 0, (VkDeviceSize)row_pitch, 0, 0 };
    VkImageDrmFormatModifierExplicitCreateInfoEXT eci = { VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_EXPLICIT_CREATE_INFO_EXT, NULL,
        0 , 1, &pl };
    VkImageCreateInfo ici = { VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, g_linmod ? (void *)&eci : NULL, 0, VK_IMAGE_TYPE_2D, (VkFormat)vkfmt,
        { w, h, 1 }, 1, 1, VK_SAMPLE_COUNT_1_BIT, g_linmod ? VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT : VK_IMAGE_TILING_LINEAR,
        VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT
            | (storage ? VK_IMAGE_USAGE_STORAGE_BIT : 0) | (render ? VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT : 0),
        VK_SHARING_MODE_EXCLUSIVE, 0, NULL, VK_IMAGE_LAYOUT_PREINITIALIZED };
    uint32_t vp = a8 ? 0 : nvmtl_vk_srgb_pair(vkfmt);
    VkFormat vfl[2] = { (VkFormat)vkfmt, (VkFormat)vp };
    VkImageFormatListCreateInfo fl = { VK_STRUCTURE_TYPE_IMAGE_FORMAT_LIST_CREATE_INFO, ici.pNext, 2, vfl };
    if (vp) {
        VkFormatProperties pp; memset(&pp, 0, sizeof pp); if (gfp) gfp(g_pdev, (VkFormat)vp, &pp);
        PFN_vkGetPhysicalDeviceImageFormatProperties2 gif = (PFN_vkGetPhysicalDeviceImageFormatProperties2)
            pvkGetInstanceProcAddr(g_inst, "vkGetPhysicalDeviceImageFormatProperties2");
        VkImageFormatListCreateInfo qfl = { VK_STRUCTURE_TYPE_IMAGE_FORMAT_LIST_CREATE_INFO, NULL, 2, vfl };
        VkPhysicalDeviceImageDrmFormatModifierInfoEXT qmi = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_DRM_FORMAT_MODIFIER_INFO_EXT, &qfl,
            0 , VK_SHARING_MODE_EXCLUSIVE, 0, NULL };
        VkPhysicalDeviceImageFormatInfo2 qi = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_FORMAT_INFO_2, g_linmod ? (void *)&qmi : (void *)&qfl,
            ici.format, ici.imageType, ici.tiling, ici.usage, VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT };
        VkImageFormatProperties2 qo; memset(&qo, 0, sizeof qo); qo.sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2;
        VkResult qr = !(pp.linearTilingFeatures & VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT) ? VK_ERROR_FORMAT_NOT_SUPPORTED
                    : gif ? gif(g_pdev, &qi, &qo) : VK_ERROR_EXTENSION_NOT_PRESENT;
        if (qr == VK_SUCCESS) { ici.flags |= VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT; ici.pNext = &fl; }
        else { nvlog("buffer texture: VkFormat %u stays single-format - its sRGB/linear view format %u is refused (linear features 0x%x, VkResult %d%s)",
                     vkfmt, vp, (unsigned)pp.linearTilingFeatures, (int)qr, gif ? "" : ", no vkGetPhysicalDeviceImageFormatProperties2"); vp = 0; }
    }
    VKCK(nvmtl_opaque_CreateImage(g_dev, &ici, NULL, &out->img), "vkCreateImage(buffer texture)");
    out->layout = VK_IMAGE_LAYOUT_PREINITIALIZED;
    out->vkflags = (uint32_t)ici.flags; out->usage = (uint32_t)ici.usage; out->vpair = vp;
    if (!g_linmod) {
        PFN_vkGetImageSubresourceLayout gsl = (PFN_vkGetImageSubresourceLayout)pvkGetInstanceProcAddr(g_inst, "vkGetImageSubresourceLayout");
        VkImageSubresource sr = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0 }; VkSubresourceLayout got; memset(&got, 0, sizeof got);
        if (gsl) gsl(g_dev, (VkImage)out->img, &sr, &got);
        if (!gsl || got.rowPitch != row_pitch || got.offset) {
            nvlog("buffer texture: LINEAR tiling picked pitch %llu offset %llu, the app's bytesPerRow is %zu - refused",
                  (unsigned long long)got.rowPitch, (unsigned long long)got.offset, row_pitch);
            pvkDestroyImage(g_dev, (VkImage)out->img, NULL); memset(out, 0, sizeof *out); return -1; }
    }
    VkMemoryRequirements mr; pvkGetImageMemoryRequirements(g_dev, (VkImage)out->img, &mr);
    uint32_t all = g_memp.memoryTypeCount >= 32 ? 0xffffffffu : ((1u << g_memp.memoryTypeCount) - 1u);
    size_t at = b->moff + offset;
    const char *why = (mr.memoryTypeBits & all) != all ? "memory types" : (mr.alignment && at % mr.alignment) ? "alignment" : NULL;
    if (why) {
        nvlog("buffer texture: refused on %s - image types 0x%x of 0x%x, bind at %zu (buffer %zu + %zu) vs alignment %llu",
              why, mr.memoryTypeBits, all, at, b->moff, offset, (unsigned long long)mr.alignment);
        pvkDestroyImage(g_dev, (VkImage)out->img, NULL); memset(out, 0, sizeof *out); return -1; }
    if (mr.size > b->size - offset)
        nvlog("buffer texture: image size %llu runs %llu B past the buffer's %zu at offset %zu (the tail of the last row, never addressed)",
              (unsigned long long)mr.size, (unsigned long long)(mr.size - (b->size - offset)), b->size, offset);
    VKCK(pvkBindImageMemory(g_dev, (VkImage)out->img, (VkDeviceMemory)b->mem, at), "vkBindImageMemory(buffer texture)");
    out->mem = b->mem; out->placed = 1; out->alloc = 0;
    VkComponentMapping swz = a8 ? (VkComponentMapping){ VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_R }
                                : (VkComponentMapping){ 0, 0, 0, 0 };
    VkImageViewCreateInfo vci = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, out->img, VK_IMAGE_VIEW_TYPE_2D,
        (VkFormat)vkfmt, swz, { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
    VKCK(nvmtl_opaque_CreateImageView(g_dev, &vci, NULL, &out->view), "vkCreateImageView(buffer texture)");

    nvlog("buffer texture: %ux%u VkFormat %u pitch %zu bound at %zu (%s, align %llu, size %llu, storage %d, render %d, view pair %u)", w, h, vkfmt, row_pitch, at,
          g_linmod ? "DRM linear" : "LINEAR", (unsigned long long)mr.alignment, (unsigned long long)mr.size, storage, render, out->vpair);
    return 0;
}

typedef struct { size_t off, len; } pool_range;
typedef struct pool_block {
    struct pool_block *next; nvk_heap heap; size_t used; uint32_t nlive;
    int vram;
    pool_range *fr; uint32_t nfr, cfr;
} pool_block;
static pool_block *g_pool;
#define NVMTL_VPOOL_BLOCK (8u << 20)
static pool_block *g_vpool;
static uint64_t g_vpool_blocks;
static pthread_mutex_t g_pool_lock = PTHREAD_MUTEX_INITIALIZER;
static size_t g_pool_align;

static size_t pool_align(void)
{
    if (g_pool_align) return g_pool_align;
    size_t a = 256;
    VkBufferCreateInfo bci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, 0, 4096, NVMTL_BUFFER_USAGE, VK_SHARING_MODE_EXCLUSIVE, 0, NULL };
    VkBuffer b = VK_NULL_HANDLE;
    if (pvkCreateBuffer(g_dev, &bci, NULL, &b) == VK_SUCCESS) {
        VkMemoryRequirements mr; pvkGetBufferMemoryRequirements(g_dev, b, &mr);
        if (mr.alignment > a) a = (size_t)mr.alignment;
        pvkDestroyBuffer(g_dev, b, NULL);
    }
    g_pool_align = a; return a;
}
static int pool_block_take(pool_block *blk, size_t len, size_t *off)
{
    for (uint32_t i = 0; i < blk->nfr; i++) {
        if (blk->fr[i].len >= len) {
            *off = blk->fr[i].off; blk->fr[i].off += len; blk->fr[i].len -= len;
            if (!blk->fr[i].len) blk->fr[i] = blk->fr[--blk->nfr];
            return 1;
        }
    }
    if (blk->used + len <= blk->heap.size) { *off = blk->used; blk->used += len; return 1; }
    return 0;
}
static void pool_block_give(pool_block *blk, size_t off, size_t len)
{
    if (off + len == blk->used) { blk->used = off; }
    else {
        for (uint32_t i = 0; i < blk->nfr; i++) {
            if (blk->fr[i].off + blk->fr[i].len == off) { blk->fr[i].len += len; return; }
            if (off + len == blk->fr[i].off) { blk->fr[i].off = off; blk->fr[i].len += len; return; }
        }
        if (blk->nfr == blk->cfr) { uint32_t c = blk->cfr ? blk->cfr * 2 : 64; pool_range *n = realloc(blk->fr, c * sizeof *n); if (!n) return; blk->fr = n; blk->cfr = c; }
        blk->fr[blk->nfr].off = off; blk->fr[blk->nfr].len = len; blk->nfr++;
    }
    if (!blk->nlive) { blk->used = 0; blk->nfr = 0; }
}
static int vpool_on(void)
{
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_SHARED_POOL_VRAM"); on = !(e && e[0] == '0');
        if (!on) nvlog("vk: small Shared buffers stay in the system-RAM pool (NVMTL_SHARED_POOL_VRAM=0)"); }
    return on && bar1_on() && !g_bar1_off;
}
static const char *pool_place_why(int vram)
{
    if (vram) return "VRAM through BAR1";
    if (!bar1_on()) return "system RAM BY DESIGN - Shared is system RAM on a discrete GPU (batch 56; NVMTL_SHARED_VRAM=1 opts in)";
    if (g_bar1_off) return "system RAM - BAR1 REFUSED a VRAM block for this process (see the REFUSED line above)";
    return "system RAM - this request never asked for VRAM (host_visible != 2)";
}
static int vpool_heap_create(size_t size, nvk_heap *out)
{
    static uint32_t bits;
    memset(out, 0, sizeof *out);
    if (!bits) {
        VkBufferCreateInfo bci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, 0, 4096, NVMTL_BUFFER_USAGE, VK_SHARING_MODE_EXCLUSIVE, 0, NULL };
        VkBuffer pb = VK_NULL_HANDLE;
        if (pvkCreateBuffer(g_dev, &bci, NULL, &pb) != VK_SUCCESS) return -1;
        VkMemoryRequirements mr; pvkGetBufferMemoryRequirements(g_dev, pb, &mr); pvkDestroyBuffer(g_dev, pb, NULL);
        bits = mr.memoryTypeBits; if (!bits) return -1;
    }
    int bt = bar1_memtype(bits, size); if (bt < 0) return -1;
    VkMemoryAllocateFlagsInfo bf = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO, NULL, VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT, 0 };
    VkMemoryAllocateInfo bi = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &bf, size, (uint32_t)bt };
    VkDeviceMemory bm = VK_NULL_HANDLE; void *bp = NULL;
    VkResult r1 = pvkAllocateMemory(g_dev, &bi, NULL, &bm);
    VkResult r2 = r1 == VK_SUCCESS ? pvkMapMemory(g_dev, bm, 0, VK_WHOLE_SIZE, 0, &bp) : r1;
    if (r2 != VK_SUCCESS || !bp) {
        if (r1 == VK_SUCCESS && r2 == VK_SUCCESS) pvkUnmapMemory(g_dev, bm);
        if (bm) pvkFreeMemory(g_dev, bm, NULL);
        g_bar1_off = 1;
        nvlog("buffer pool: VRAM block REFUSED (alloc %d, map %d) at %zu BAR bytes - small Shared buffers take system RAM for this process", r1, r2, g_bar1_bytes);
        return -1; }
    out->mem = bm; out->map = bp; out->size = size; out->memtype = (uint32_t)bt; out->host_visible = 1;
    g_alloc_bytes += size; g_bar1_bytes += size;
    return 0;
}
static int pool_buffer_create_in(int vram, size_t size, nvk_buffer *out)
{
    size_t al = pool_align(), len = (size + al - 1) / al * al; if (!len) len = al;
    pthread_mutex_lock(&g_pool_lock);
    pool_block **head = vram ? &g_vpool : &g_pool;
    pool_block *blk = *head; size_t off = 0; int got = 0;
    for (; blk; blk = blk->next) if (pool_block_take(blk, len, &off)) { got = 1; break; }
    if (!got) {
        pool_block *nb = calloc(1, sizeof *nb);
        if (!nb || (vram ? vpool_heap_create(NVMTL_VPOOL_BLOCK, &nb->heap) : nvmtl_vk_heap_create(NVMTL_POOL_BLOCK, 1, &nb->heap))) { free(nb); pthread_mutex_unlock(&g_pool_lock); return -1; }
        nb->vram = vram; nb->next = *head; *head = nb; g_pool_blocks++; if (vram) g_vpool_blocks++;
        nvlog("buffer pool: block #%llu mapped (%zu MiB, %s; %llu VRAM blocks, memory type %u); %llu pooled buffers live in %llu bytes of ranges",
              (unsigned long long)g_pool_blocks, nb->heap.size >> 20, pool_place_why(vram), (unsigned long long)g_vpool_blocks,
              nb->heap.memtype, (unsigned long long)g_pool_live, (unsigned long long)g_pool_bytes);
        blk = nb; if (!pool_block_take(blk, len, &off)) { pthread_mutex_unlock(&g_pool_lock); return -1; }
    }
    blk->nlive++; g_pool_live++; g_pool_bytes += len;
    pthread_mutex_unlock(&g_pool_lock);
    nvk_buffer pb; memset(&pb, 0, sizeof pb);
    if (nvmtl_vk_buffer_create_placed(size, &blk->heap, off, &pb)) {
        pthread_mutex_lock(&g_pool_lock); blk->nlive--; g_pool_live--; g_pool_bytes -= len; pool_block_give(blk, off, len); pthread_mutex_unlock(&g_pool_lock);
        return -1;
    }
    *out = pb; out->pool = blk; out->poff = off; out->alloc = len; out->size = size;
    if (!out->map && blk->heap.map) out->map = (uint8_t *)blk->heap.map + off;
    if ((g_buf_made & 0xFFF) == 0) nvlog("buffer census: %llu made, %llu gone, %llu live, %zu bytes allocated; pool %llu blocks, %llu pooled live, %llu bytes in ranges",
        (unsigned long long)g_buf_made, (unsigned long long)g_buf_gone, (unsigned long long)(g_buf_made - g_buf_gone), g_alloc_bytes,
        (unsigned long long)g_pool_blocks, (unsigned long long)g_pool_live, (unsigned long long)g_pool_bytes);
    return 0;
}
static int pool_buffer_create(size_t size, nvk_buffer *out) { return pool_buffer_create_in(0, size, out); }
static void pool_buffer_release(nvk_buffer *b)
{
    pool_block *blk = (pool_block *)b->pool; if (!blk) return;
    pthread_mutex_lock(&g_pool_lock);
    blk->nlive--; g_pool_live--; g_pool_bytes -= b->alloc;
    pool_block_give(blk, b->poff, b->alloc);
    if (!blk->nlive) {
        pool_block **head = blk->vram ? &g_vpool : &g_pool;
        uint32_t empties = 0; for (pool_block *p = *head; p; p = p->next) if (!p->nlive) empties++;
        if (empties > NVMTL_POOL_KEEP) {
            pool_block **pp = head; while (*pp && *pp != blk) pp = &(*pp)->next;
            if (*pp) { *pp = blk->next; const size_t hs = blk->heap.size; const int v = blk->vram;
                nvmtl_vk_heap_destroy(&blk->heap); free(blk->fr); free(blk); g_pool_blocks--;
                if (v) { g_vpool_blocks--; g_bar1_bytes -= hs; } }
        }
    }
    pthread_mutex_unlock(&g_pool_lock);
    b->pool = NULL;
}

int nvmtl_vk_image_placed_fits(const nvk_image *img, size_t len)
{
    if (g_state != 1 || !img || !img->img) return 0;
    VkMemoryRequirements mr; pvkGetImageMemoryRequirements(g_dev, (VkImage)img->img, &mr);
    if ((size_t)mr.size <= len) return 1;
    nvlog("vk: placed image %ux%u fmt %u needs %llu B but its range is %zu B - REFUSED (the size query answered for a different image)",
          img->w, img->h, img->fmt, (unsigned long long)mr.size, len);
    return 0;
}
static int nvmtl_vk_image_create_typed_placed_lifetime_body(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, int storage, uint32_t mtl_type, uint32_t layers, nvk_heap *heap, size_t offset, nvk_image *out);
int nvmtl_vk_image_create_typed_placed(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, int storage, uint32_t mtl_type, uint32_t layers, nvk_heap *heap, size_t offset, nvk_image *out)
{
    nvk_image pending = {0};
    int result = nvmtl_vk_image_create_typed_placed_lifetime_body(w, h, vkfmt, bpp, a8, mips, storage, mtl_type, layers, heap, offset, &pending);
    if (result) { nvmtl_failed_image(&pending); memset(out, 0, sizeof *out); }
    else { nvmtl_commit_image(&pending); *out = pending; }
    return result;
}
static int nvmtl_vk_image_create_typed_placed_lifetime_body(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, int storage, uint32_t mtl_type, uint32_t layers, nvk_heap *heap, size_t offset, nvk_image *out)
{
    if (!layers || (mtl_type < 2 && h != 1) || mtl_type == 4 || mtl_type > 7
        || ((mtl_type == 0 || mtl_type == 2) && layers != 1) || (mtl_type == 5 && layers != 6) || (mtl_type == 6 && layers % 6)) {
        nvlog("image_create: textureType %u with %u layer(s) is not carried", mtl_type, layers); return -1; }
    if (nvmtl_vk_init() || !heap || !heap->mem) return -1;
    if (!mips) mips = 1;
    VkFormat fmt = (VkFormat)vkfmt;
    storage = storage && !a8 && nvmtl_vk_format_is_storage(vkfmt);
    memset(out, 0, sizeof *out); out->w = w; out->h = h; out->mips = mips; out->fmt = vkfmt; out->bpp = bpp ? bpp : 4;
    out->storage = storage ? 1 : 0; out->mtl_type = mtl_type; out->layers = layers;
    { uint32_t bb = 0; if (nvmtl_vk_format_block(vkfmt, &out->bw, &out->bh, &bb)) { out->bpp = bb; out->storage = 0; storage = 0; } }
    VkImageCreateInfo ici = { VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, NULL, (mtl_type == 5 || mtl_type == 6) ? VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT : 0, mtl_type == 7 ? VK_IMAGE_TYPE_3D : mtl_type < 2 ? VK_IMAGE_TYPE_1D : VK_IMAGE_TYPE_2D,
        fmt, { w, h, mtl_type == 7 ? layers : 1 }, mips, mtl_type == 7 ? 1 : layers, VK_SAMPLE_COUNT_1_BIT, VK_IMAGE_TILING_OPTIMAL,
        (out->bw > 1 ? 0 : VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT) | VK_IMAGE_USAGE_TRANSFER_SRC_BIT |
        VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_SAMPLED_BIT | (storage ? VK_IMAGE_USAGE_STORAGE_BIT : 0),
        VK_SHARING_MODE_EXCLUSIVE, 0, NULL, VK_IMAGE_LAYOUT_UNDEFINED };
    nvmtl_vk_image_mutable(&ici, out);
    VKCK(nvmtl_opaque_CreateImage(g_dev, &ici, NULL, &out->img), "vkCreateImage(placed)");
    VkMemoryRequirements mr; pvkGetImageMemoryRequirements(g_dev, out->img, &mr);
    if (placed_ok(&mr, heap, offset, "placed image")) { pvkDestroyImage(g_dev, (VkImage)out->img, NULL); memset(out, 0, sizeof *out); return -1; }
    VKCK(pvkBindImageMemory(g_dev, out->img, (VkDeviceMemory)heap->mem, offset), "vkBindImageMemory(placed)");
    out->mem = heap->mem; out->placed = 1; out->alloc = 0;
    VkComponentMapping swz = a8 ? (VkComponentMapping){ VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_R }
                                : (VkComponentMapping){ 0, 0, 0, 0 };
    VkImageViewType viewType = mtl_type == 0 ? VK_IMAGE_VIEW_TYPE_1D : mtl_type == 1 ? VK_IMAGE_VIEW_TYPE_1D_ARRAY : mtl_type == 3 ? VK_IMAGE_VIEW_TYPE_2D_ARRAY
                             : mtl_type == 5 ? VK_IMAGE_VIEW_TYPE_CUBE : mtl_type == 6 ? VK_IMAGE_VIEW_TYPE_CUBE_ARRAY : mtl_type == 7 ? VK_IMAGE_VIEW_TYPE_3D : VK_IMAGE_VIEW_TYPE_2D;
    VkImageViewCreateInfo vci = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, out->img, viewType,
        fmt, swz, { VK_IMAGE_ASPECT_COLOR_BIT, 0, mips, 0, mtl_type == 7 ? 1 : layers } };
    VKCK(nvmtl_opaque_CreateImageView(g_dev, &vci, NULL, &out->view), "vkCreateImageView(placed)");

    return 0;
}
static int nvmtl_vk_depth_create_ex_placed_lifetime_body(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t aspect, nvk_heap *heap, size_t offset, nvk_image *out);
int nvmtl_vk_depth_create_ex_placed(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t aspect, nvk_heap *heap, size_t offset, nvk_image *out)
{
    nvk_image pending = {0};
    int result = nvmtl_vk_depth_create_ex_placed_lifetime_body(w, h, vkfmt, aspect, heap, offset, &pending);
    if (result) { nvmtl_failed_image(&pending); memset(out, 0, sizeof *out); }
    else { nvmtl_commit_image(&pending); *out = pending; }
    return result;
}
static int nvmtl_vk_depth_create_ex_placed_lifetime_body(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t aspect, nvk_heap *heap, size_t offset, nvk_image *out)
{
    if (nvmtl_vk_init() || !heap || !heap->mem) return -1;
    memset(out, 0, sizeof *out); out->w = w; out->h = h; out->fmt = vkfmt; out->bpp = 4; out->mtl_type = 2; out->layers = 1;
    VkImageCreateInfo ici = { VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, NULL, 0, VK_IMAGE_TYPE_2D,
        (VkFormat)vkfmt, { w, h, 1 }, 1, 1, VK_SAMPLE_COUNT_1_BIT, VK_IMAGE_TILING_OPTIMAL,
        NVMTL_DEPTH_USAGE, VK_SHARING_MODE_EXCLUSIVE, 0, NULL, VK_IMAGE_LAYOUT_UNDEFINED };
    VKCK(nvmtl_opaque_CreateImage(g_dev, &ici, NULL, &out->img), "vkCreateImage(placed depth)");
    out->usage = (uint32_t)ici.usage;
    VkMemoryRequirements mr; pvkGetImageMemoryRequirements(g_dev, out->img, &mr);
    if (placed_ok(&mr, heap, offset, "placed depth image")) { pvkDestroyImage(g_dev, (VkImage)out->img, NULL); memset(out, 0, sizeof *out); return -1; }
    VKCK(pvkBindImageMemory(g_dev, out->img, (VkDeviceMemory)heap->mem, offset), "vkBindImageMemory(placed depth)");
    out->mem = heap->mem; out->placed = 1; out->alloc = 0;
    VkImageViewCreateInfo vci = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, out->img, VK_IMAGE_VIEW_TYPE_2D,
        (VkFormat)vkfmt, { 0 }, { (VkImageAspectFlags)aspect, 0, 1, 0, 1 } };
    VKCK(nvmtl_opaque_CreateImageView(g_dev, &vci, NULL, &out->view), "vkCreateImageView(placed depth)");
    return 0;
}
int nvmtl_vk_buffer_size_align(size_t size, size_t *out_size, size_t *out_align)
{
    if (nvmtl_vk_init()) return -1;
    VkBufferCreateInfo bci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, 0, size ? size : 1, NVMTL_BUFFER_USAGE, VK_SHARING_MODE_EXCLUSIVE, 0, NULL };
    VkBuffer b = VK_NULL_HANDLE;
    VKCK(pvkCreateBuffer(g_dev, &bci, NULL, &b), "vkCreateBuffer(size query)");
    VkMemoryRequirements mr; pvkGetBufferMemoryRequirements(g_dev, b, &mr); pvkDestroyBuffer(g_dev, b, NULL);
    *out_size = (size_t)mr.size; *out_align = (size_t)(mr.alignment ? mr.alignment : 256);
    return 0;
}
static int nvmtl_vk_image_size_align_uncached(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t mips, uint32_t mtl_type, uint32_t layers, int depth_attachment, int storage, size_t *out_size, size_t *out_align)
{
    if (nvmtl_vk_init()) return -1;
    if (!mips) mips = 1;
    if (!layers) layers = 1;
    uint32_t qbw = 1, qbh = 1, qbb = 0; int compressed = nvmtl_vk_format_block(vkfmt, &qbw, &qbh, &qbb);
    storage = storage && !depth_attachment && !compressed && nvmtl_vk_format_is_storage(vkfmt);
    VkImageCreateInfo ici = { VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, NULL, (!depth_attachment && (mtl_type == 5 || mtl_type == 6)) ? VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT : 0, (!depth_attachment && mtl_type == 7) ? VK_IMAGE_TYPE_3D : (!depth_attachment && mtl_type < 2) ? VK_IMAGE_TYPE_1D : VK_IMAGE_TYPE_2D,
        (VkFormat)vkfmt, { w, h, (!depth_attachment && mtl_type == 7) ? layers : 1 }, depth_attachment ? 1 : mips, (depth_attachment || mtl_type == 7) ? 1 : layers, VK_SAMPLE_COUNT_1_BIT, VK_IMAGE_TILING_OPTIMAL,
        depth_attachment ? NVMTL_DEPTH_USAGE
                         : ((compressed ? 0 : VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT) | VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT |
                            VK_IMAGE_USAGE_SAMPLED_BIT | (storage ? VK_IMAGE_USAGE_STORAGE_BIT : 0)),
        VK_SHARING_MODE_EXCLUSIVE, 0, NULL, VK_IMAGE_LAYOUT_UNDEFINED };
    VkImage im = VK_NULL_HANDLE;
    if (!depth_attachment) ici.flags |= VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT;
    if (!depth_attachment && nvmtl_vk_cube_compat(mtl_type, w, h, layers))
        ici.flags |= VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT;
    if (!depth_attachment && ici.imageType == VK_IMAGE_TYPE_3D && (ici.usage & VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT)
        && nvmtl_vk_3d_slice_ok(ici.format, ici.usage))
        ici.flags |= VK_IMAGE_CREATE_2D_ARRAY_COMPATIBLE_BIT;
    VKCK(nvmtl_typed_CreateImage(g_dev, &ici, NULL, &im), "vkCreateImage(size query)");
    VkMemoryRequirements mr; pvkGetImageMemoryRequirements(g_dev, im, &mr); pvkDestroyImage(g_dev, im, NULL);
    *out_size = (size_t)mr.size; *out_align = (size_t)(mr.alignment ? mr.alignment : 256);
    return 0;
}

typedef struct {
    VkDevice dev;
    uint32_t key[8];
    size_t size, align;
    int valid;
} nvmtl_sizequery_entry;
static nvmtl_sizequery_entry nvmtl_sizequeries[256];
static unsigned nvmtl_sizequery_next;
static pthread_mutex_t nvmtl_sizequery_lock = PTHREAD_MUTEX_INITIALIZER;
static _Atomic unsigned long g_sq_hit, g_sq_miss;
int nvmtl_vk_image_size_align(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t mips, uint32_t mtl_type, uint32_t layers, int depth_attachment, int storage, size_t *out_size, size_t *out_align)
{
    if (nvmtl_vk_init()) return -1;
    const uint32_t key[8] = {w, h, vkfmt, mips, mtl_type, layers,
                            (uint32_t)depth_attachment, (uint32_t)storage};
    const char *disable = getenv("NVMTL_NO_SIZEQUERY_CACHE");
    const int enabled = !(disable && disable[0] && disable[0] != '0');
    if (enabled) {
        pthread_mutex_lock(&nvmtl_sizequery_lock);
        for (unsigned i = 0; i < 256; ++i) {
            const nvmtl_sizequery_entry *e = &nvmtl_sizequeries[i];
            if (e->valid && e->dev == g_dev && !memcmp(e->key, key, sizeof key)) {
                *out_size = e->size; *out_align = e->align;
                pthread_mutex_unlock(&nvmtl_sizequery_lock);
                unsigned long hn = ++g_sq_hit;
                if (!(hn & (hn - 1))) nvlog("sizequery: HIT %ux%u fmt %u -> %zu/%zu [%lu hit, %lu miss]", w, h, vkfmt, *out_size, *out_align, hn, (unsigned long)g_sq_miss);
                return 0;
            }
        }
        pthread_mutex_unlock(&nvmtl_sizequery_lock);
    }
    const int rc = nvmtl_vk_image_size_align_uncached(w, h, vkfmt, mips, mtl_type,
                      layers, depth_attachment, storage, out_size, out_align);
    unsigned long mn = ++g_sq_miss;
    if (!(mn & (mn - 1))) nvlog("sizequery: %s %ux%u fmt %u rc %d [%lu hit, %lu miss]", enabled ? "miss" : "OFF", w, h, vkfmt, rc, (unsigned long)g_sq_hit, mn);
    if (!rc && enabled) {
        pthread_mutex_lock(&nvmtl_sizequery_lock);
        nvmtl_sizequery_entry *e = &nvmtl_sizequeries[nvmtl_sizequery_next++ % 256];
        e->dev = g_dev; memcpy(e->key, key, sizeof key);
        e->size = *out_size; e->align = *out_align; e->valid = 1;
        pthread_mutex_unlock(&nvmtl_sizequery_lock);
    }
    return rc;
}

int nvmtl_vk_cmd_copy_image_sub(nvk_cmdbuf *c, nvk_image *src, uint32_t sl, uint32_t ss, uint32_t sx, uint32_t sy,
                                nvk_image *dst, uint32_t dl, uint32_t ds, uint32_t dx, uint32_t dy, uint32_t w, uint32_t h)
{
    if (!c || !src || !dst || !w || !h || src->fmt != dst->fmt
        || (src->samples ? src->samples : 1) != (dst->samples ? dst->samples : 1)
        || ss >= (src->layers ? src->layers : 1) || ds >= (dst->layers ? dst->layers : 1)
        || sl >= (src->mips ? src->mips : 1) || dl >= (dst->mips ? dst->mips : 1)) {
        nvlog("copy_image_sub: incompatible format/sample count or invalid subresource");
        return -1;
    }
    const uint32_t sw = nvmtl_lvl(src->w, sl), sh = nvmtl_lvl(src->h, sl);
    const uint32_t dw = nvmtl_lvl(dst->w, dl), dh = nvmtl_lvl(dst->h, dl);
    if (sx > sw || w > sw - sx || sy > sh || h > sh - sy
        || dx > dw || w > dw - dx || dy > dh || h > dh - dy) {
        nvlog("copy_image_sub: region outside selected mip extent");
        return -1;
    }

    const VkImageAspectFlags aspects = nvmtl_barrier_aspect(src);
    VkImageCopy regions[2]; uint32_t count = 0;
    for (VkImageAspectFlags aspect = VK_IMAGE_ASPECT_COLOR_BIT;
         aspect <= VK_IMAGE_ASPECT_STENCIL_BIT; aspect <<= 1) {
        if (!(aspects & aspect)) continue;
        regions[count++] = (VkImageCopy){ { aspect, sl, ss, 1 }, { (int32_t)sx, (int32_t)sy, 0 },
                                         { aspect, dl, ds, 1 }, { (int32_t)dx, (int32_t)dy, 0 }, { w, h, 1 } };
    }
    nvmtl_vk_cmd_barrier(c);
    img_to(c, src, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_ACCESS_MEMORY_WRITE_BIT | VK_ACCESS_MEMORY_READ_BIT, VK_ACCESS_TRANSFER_READ_BIT,
           VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
    img_to(c, dst, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_ACCESS_MEMORY_WRITE_BIT | VK_ACCESS_MEMORY_READ_BIT, VK_ACCESS_TRANSFER_WRITE_BIT,
           VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
    pvkCmdCopyImage(c->cb, (VkImage)src->img, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                   (VkImage)dst->img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, count, regions);
    img_to(c, dst, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_SHADER_READ_BIT,
           VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT);
    nvmtl_vk_cmd_barrier(c);
    return 0;
}
int nvmtl_vk_image_view_create_range(nvk_image *img, uint32_t vkfmt, int a8, uint32_t baseLevel, uint32_t levelCount,
                                     uint32_t baseLayer, uint32_t layerCount, uint32_t mtl_view_type, void **out_view)
{
    nvk_image format_image = {.fmt = vkfmt};
    return nvmtl_vk_image_view_create_range_aspect(img, vkfmt, a8, nvmtl_copy_aspect(&format_image), baseLevel, levelCount, baseLayer, layerCount, mtl_view_type, out_view);
}
int nvmtl_vk_image_view_create_range_aspect(nvk_image *img, uint32_t vkfmt, int a8, uint32_t aspect, uint32_t baseLevel, uint32_t levelCount,
                                     uint32_t baseLayer, uint32_t layerCount, uint32_t mtl_view_type, void **out_view)
{
    if (!img || !out_view || nvmtl_vk_init()) return -1;
    nvk_image format_image = {.fmt = vkfmt};
    if (!aspect || (aspect & ~nvmtl_barrier_aspect(&format_image))) return -1;
    const uint32_t mips = img->mips ? img->mips : 1, layers = img->layers ? img->layers : 1;
    if (baseLevel >= mips || baseLayer >= layers) return -1;
    if (!levelCount) levelCount = mips - baseLevel;
    if (!layerCount) layerCount = layers - baseLayer;
    if (levelCount > mips - baseLevel) { nvlog("image_view_create(range): levels [%u,%u) is outside the %u level(s)", baseLevel, baseLevel + levelCount, mips); return -1; }
    if (layerCount > layers - baseLayer) { nvlog("image_view_create(range): slices [%u,%u) is outside the %u slice(s)", baseLayer, baseLayer + layerCount, layers); return -1; }
    VkImageViewType vt;
    switch (mtl_view_type) {
        case 0: vt = VK_IMAGE_VIEW_TYPE_1D; break;
        case 1: vt = VK_IMAGE_VIEW_TYPE_1D_ARRAY; break;
        case 2: vt = VK_IMAGE_VIEW_TYPE_2D; if (layerCount != 1) { nvlog("image_view_create(range): a 2D view of %u slices", layerCount); return -1; } break;
        case 3: vt = VK_IMAGE_VIEW_TYPE_2D_ARRAY; break;
        case 4: vt = VK_IMAGE_VIEW_TYPE_2D; if ((img->mtl_type != 4 && img->mtl_type != 8) || layerCount != 1 || baseLevel != 0 || levelCount != 1) { nvlog("image_view_create(range): a multisample view of a %u-typed image / %u slices", img->mtl_type, layerCount); return -1; } break;
        case 8: vt = VK_IMAGE_VIEW_TYPE_2D_ARRAY; if (img->mtl_type != 8 || baseLevel != 0 || levelCount != 1) return -1; break;
        case 5: vt = VK_IMAGE_VIEW_TYPE_CUBE; if (layerCount != 6) { nvlog("image_view_create(range): a cube view of %u slices", layerCount); return -1; } break;
        case 6: vt = VK_IMAGE_VIEW_TYPE_CUBE_ARRAY; if (layerCount % 6) { nvlog("image_view_create(range): a cube-array view of %u slices", layerCount); return -1; } break;
        case 7: vt = VK_IMAGE_VIEW_TYPE_3D; if (img->mtl_type != 7) { nvlog("image_view_create(range): a 3D view of a non-3D image"); return -1; } layerCount = 1; break;
        default: nvlog("image_view_create(range): view textureType %u is not carried", mtl_view_type); return -1;
    }
    VkComponentMapping swz = a8 ? (VkComponentMapping){ VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_ZERO, VK_COMPONENT_SWIZZLE_R }
                                : (VkComponentMapping){ 0, 0, 0, 0 };
    VkImageViewCreateInfo vci = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, (VkImage)img->img, vt, (VkFormat)vkfmt, swz,
        { (VkImageAspectFlags)aspect, baseLevel, levelCount, baseLayer, layerCount } };
    VkImageViewUsageCreateInfo uci;
    int vg = nvmtl_vk_view_guard(img, vkfmt, a8, vci.viewType, vci.subresourceRange.levelCount, &uci, "image_view_create(range)");
    if (vg < 0) return -1;
    if (vg > 0) vci.pNext = &uci;
    VkImageView v = VK_NULL_HANDLE;
    VKCK(pvkCreateImageView(g_dev, &vci, NULL, &v), "vkCreateImageView(range)");
    *out_view = v;
    return 0;
}

static void nvmtl_destroy_pipe_once(VkPipeline *seen, uint32_t *nseen, uint32_t cap, VkPipeline h)
{
    if (!h) return;
    for (uint32_t i = 0; i < *nseen; i++) if (seen[i] == h) return;
    if (*nseen < cap) seen[(*nseen)++] = h;
    pvkDestroyPipeline(g_dev, h, NULL);
}
void nvmtl_vk_pipeline_destroy(nvk_pipeline *p)
{
    if (!p || !g_dev) return;
    VkPipeline seen[64]; uint32_t nseen = 0;
    pthread_mutex_lock(&g_var_lock);
    for (uint32_t i = 0; i < p->nvar; i++) nvmtl_destroy_pipe_once(seen, &nseen, 64, (VkPipeline)p->var[i].pipe);
    free(p->var); p->var = NULL; p->nvar = p->capvar = 0;
    pthread_mutex_unlock(&g_var_lock);
    nvmtl_destroy_pipe_once(seen, &nseen, 64, (VkPipeline)p->pipe);
    pthread_mutex_lock(&g_ag_lock);
    nvk_pipeline *safe = (nvk_pipeline *)p->ag_safe; p->ag_safe = NULL;
    pthread_mutex_unlock(&g_ag_lock);
    if (safe) { nvmtl_vk_pipeline_destroy(safe); free(safe); }
    pthread_mutex_lock(&g_rpv_lock);
    for (uint32_t i = 1; i < 4; i++) if (p->rpv[i] && p->rpv[i] != p->rp) pvkDestroyRenderPass(g_dev, (VkRenderPass)p->rpv[i], NULL);
    for (uint32_t i = 0; i < p->nrpx; i++) if (p->rpx[i] && p->rpx[i] != p->rp) pvkDestroyRenderPass(g_dev, (VkRenderPass)p->rpx[i], NULL);
    free(p->rpx); free(p->rpxk); p->rpx = NULL; p->rpxk = NULL; p->nrpx = p->crpx = 0;
    if (p->shape_identity) {
        uint32_t w = 0;
        for (uint32_t i = 0; i < g_nsrp; i++) {
            if (g_srp[i].identity == p->shape_identity) { if (g_srp[i].rp) pvkDestroyRenderPass(g_dev, (VkRenderPass)g_srp[i].rp, NULL); }
            else g_srp[w++] = g_srp[i];
        }
        g_nsrp = w;
    }
    pthread_mutex_unlock(&g_rpv_lock);
    if (p->rp) pvkDestroyRenderPass(g_dev, (VkRenderPass)p->rp, NULL);
    if (p->mod) pvkDestroyShaderModule(g_dev, (VkShaderModule)p->mod, NULL);
    if (p->layout) pvkDestroyPipelineLayout(g_dev, (VkPipelineLayout)p->layout, NULL);
    free(p->ag_src);
    memset(p, 0, sizeof *p);
    static _Atomic unsigned long long freed;
    const unsigned long long k = atomic_fetch_add(&freed, 1) + 1;
    if (!(k & (k - 1))) nvlog("psofree: %llu pipeline state(s) freed after their last command buffer completed", k);
}
