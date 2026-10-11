// vkprobe: lists every Vulkan physical device the loader finds and creates a logical device on each.
// Used to prove the macOS builds of Mesa's AMD (RADV) and Intel (ANV) drivers load and run, chip by chip,
// with RADV_FORCE_FAMILY / INTEL_STUB_GPU selecting the GPU when no hardware is present.
#include <stdio.h>
#include <stdlib.h>
#include <vulkan/vulkan.h>
#include "fill_spv.h"

int main(void)
{
    VkApplicationInfo app = { VK_STRUCTURE_TYPE_APPLICATION_INFO, NULL, "vkprobe", 1, NULL, 0, VK_API_VERSION_1_3 };
    VkInstanceCreateInfo ici = { VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, NULL, 0, &app, 0, NULL, 0, NULL };
    VkInstance inst;
    VkResult r = vkCreateInstance(&ici, NULL, &inst);
    if (r != VK_SUCCESS) { printf("vkCreateInstance failed: %d\n", r); return 1; }
    uint32_t n = 0;
    vkEnumeratePhysicalDevices(inst, &n, NULL);
    if (!n) { printf("no physical devices\n"); vkDestroyInstance(inst, NULL); return 2; }
    VkPhysicalDevice *pd = calloc(n, sizeof *pd);
    vkEnumeratePhysicalDevices(inst, &n, pd);
    int failed = 0;
    for (uint32_t i = 0; i < n; i++) {
        VkPhysicalDeviceProperties p;
        vkGetPhysicalDeviceProperties(pd[i], &p);
        printf("device %u: %s | vendor 0x%04x device 0x%04x | API %u.%u.%u | driver 0x%x\n", i, p.deviceName, p.vendorID,
               p.deviceID, VK_API_VERSION_MAJOR(p.apiVersion), VK_API_VERSION_MINOR(p.apiVersion),
               VK_API_VERSION_PATCH(p.apiVersion), p.driverVersion);
        float prio = 1.0f;
        VkDeviceQueueCreateInfo q = { VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, NULL, 0, 0, 1, &prio };
        VkDeviceCreateInfo dci = { VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, NULL, 0, 1, &q, 0, NULL, 0, NULL, NULL };
        VkDevice dev;
        r = vkCreateDevice(pd[i], &dci, NULL, &dev);
        printf("  vkCreateDevice: %s (%d)\n", r == VK_SUCCESS ? "OK" : "FAILED", r);
        if (r != VK_SUCCESS) { failed++; continue; }
        /* Compile a real compute pipeline: proves the chip's shader compiler (ACO / Intel brw/elk) runs on macOS. */
        VkShaderModuleCreateInfo smi = { VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, NULL, 0, fill_spv_len, (const uint32_t *)fill_spv };
        VkShaderModule sm;
        VkDescriptorSetLayoutBinding b = { 0, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT, NULL };
        VkDescriptorSetLayoutCreateInfo dli = { VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, NULL, 0, 1, &b };
        VkDescriptorSetLayout dsl;
        VkPipelineLayout pl;
        VkPipeline pipe;
        r = vkCreateShaderModule(dev, &smi, NULL, &sm);
        if (r == VK_SUCCESS) r = vkCreateDescriptorSetLayout(dev, &dli, NULL, &dsl);
        if (r == VK_SUCCESS) {
            VkPipelineLayoutCreateInfo pli = { VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, NULL, 0, 1, &dsl, 0, NULL };
            r = vkCreatePipelineLayout(dev, &pli, NULL, &pl);
        }
        if (r == VK_SUCCESS) {
            VkComputePipelineCreateInfo cpi = { VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO, NULL, 0,
                { VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0, VK_SHADER_STAGE_COMPUTE_BIT, sm, "main", NULL },
                pl, VK_NULL_HANDLE, 0 };
            r = vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &cpi, NULL, &pipe);
            if (r == VK_SUCCESS) vkDestroyPipeline(dev, pipe, NULL);
            vkDestroyPipelineLayout(dev, pl, NULL);
            vkDestroyDescriptorSetLayout(dev, dsl, NULL);
        }
        printf("  compute pipeline: %s (%d)\n", r == VK_SUCCESS ? "OK" : "FAILED", r);
        if (r != VK_SUCCESS) failed++;
        vkDestroyShaderModule(dev, sm, NULL);
        vkDestroyDevice(dev, NULL);
    }
    free(pd);
    vkDestroyInstance(inst, NULL);
    return failed ? 3 : 0;
}
