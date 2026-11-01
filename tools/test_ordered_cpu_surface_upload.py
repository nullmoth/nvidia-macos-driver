#!/usr/bin/env python3
"""Exercise the actual ordered barrier used by the async CPU surface-upload fallback."""
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
source = (root / "plugin/nvmtl_vk.c").read_text()
ordered_start = source.index("// NVMTL_SURFIN_ORDERED")
ordered_end = source.index("\n// Deferred surface-in uploads retain", ordered_start)
ordered = source[ordered_start:ordered_end]
helper_start = source.index("static void nvmtl_upload_region_to_transfer(")
helper_end = source.index("\nstatic void nvmtl_private_depth_layout", helper_start)
helper = source[helper_start:helper_end]
region_start = source.index("static int nvmtl_vk_image_write_region_level_layer_body(")
region_end = source.index("\nint nvmtl_vk_image_write_region_level_layer(", region_start)
assert "nvmtl_upload_region_to_transfer(c, img);" in source[region_start:region_end]

fixture = r'''
#include <assert.h>
#include <mach/mach.h>
#include <stdint.h>
#include <stdio.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <vulkan/vulkan_core.h>
#include "nvmtl_vk.h"

static __thread int g_up_force_async;
static unsigned barrier_calls, img_to_calls;
static VkImageMemoryBarrier seen;
static VkPipelineStageFlags seen_src, seen_dst;
static VkImageLayout old_layout;
static VkAccessFlags src_access, dst_access;
static VkPipelineStageFlags src_stage, dst_stage;
static void nvlog(const char *fmt, ...) { (void)fmt; }
static VkImageAspectFlags nvmtl_barrier_aspect(const nvk_image *img) { (void)img; return VK_IMAGE_ASPECT_COLOR_BIT; }
static void pvkCmdPipelineBarrier(VkCommandBuffer cb, VkPipelineStageFlags src, VkPipelineStageFlags dst,
    VkDependencyFlags flags, uint32_t memory_count, const VkMemoryBarrier *memory,
    uint32_t buffer_count, const VkBufferMemoryBarrier *buffers,
    uint32_t image_count, const VkImageMemoryBarrier *images) {
    (void)cb; (void)flags; (void)memory_count; (void)memory; (void)buffer_count; (void)buffers;
    assert(image_count == 1 && images); seen=*images; seen_src=src; seen_dst=dst; barrier_calls++;
}
static void img_to(nvk_cmdbuf *cb, nvk_image *img, VkImageLayout to,
                   VkAccessFlags srcA, VkAccessFlags dstA, VkPipelineStageFlags srcS, VkPipelineStageFlags dstS) {
    (void)cb; img_to_calls++; old_layout=(VkImageLayout)img->layout;
    src_access=srcA; dst_access=dstA; src_stage=srcS; dst_stage=dstS; img->layout=to;
}
'''

checks = r'''
static void ordered_case(VkImageLayout layout) {
    barrier_calls=img_to_calls=0; g_up_force_async=1;
    nvk_cmdbuf cb={.cb=(void *)(uintptr_t)1};
    nvk_image image={.img=(void *)(uintptr_t)2,.layout=layout,.mips=4,.layers=3};
    nvmtl_upload_region_to_transfer(&cb,&image);
    assert(barrier_calls==1 && img_to_calls==0);
    assert(seen_src==VK_PIPELINE_STAGE_ALL_COMMANDS_BIT && seen_dst==VK_PIPELINE_STAGE_TRANSFER_BIT);
    assert(seen.srcAccessMask & VK_ACCESS_MEMORY_READ_BIT);
    assert(seen.srcAccessMask & VK_ACCESS_MEMORY_WRITE_BIT);
    assert(seen.dstAccessMask & VK_ACCESS_TRANSFER_WRITE_BIT);
    assert(seen.oldLayout==layout && seen.newLayout==VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL);
    assert(seen.subresourceRange.baseMipLevel==0 && seen.subresourceRange.levelCount==4);
    assert(seen.subresourceRange.baseArrayLayer==0 && seen.subresourceRange.layerCount==3);
    assert(image.layout==VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL);
}
static void ordinary_case(void) {
    barrier_calls=img_to_calls=0; g_up_force_async=0;
    nvk_cmdbuf cb={.cb=(void *)(uintptr_t)1};
    nvk_image image={.img=(void *)(uintptr_t)2,.layout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,.mips=4,.layers=3};
    nvmtl_upload_region_to_transfer(&cb,&image);
    assert(!barrier_calls && img_to_calls==1);
    assert(old_layout==VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    assert(src_access==0 && dst_access==VK_ACCESS_TRANSFER_WRITE_BIT);
    assert(src_stage==VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT && dst_stage==VK_PIPELINE_STAGE_TRANSFER_BIT);
    assert(image.layout==VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL);
}
static void ordered_off_case(void) {
    barrier_calls=img_to_calls=0; g_up_force_async=1;
    nvk_cmdbuf cb={.cb=(void *)(uintptr_t)1};
    nvk_image image={.img=(void *)(uintptr_t)2,.layout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,.mips=4,.layers=3};
    nvmtl_upload_region_to_transfer(&cb,&image);
    assert(!barrier_calls && img_to_calls==1);
    assert(old_layout==VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    assert(src_access==0 && dst_access==VK_ACCESS_TRANSFER_WRITE_BIT);
    assert(src_stage==VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT && dst_stage==VK_PIPELINE_STAGE_TRANSFER_BIT);
    assert(image.layout==VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL);
}
int main(int argc,char **argv) {
    assert(argc==2);
    if (!strcmp(argv[1],"async")) ordered_case(VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    else if (!strcmp(argv[1],"same")) ordered_case(VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL);
    else if (!strcmp(argv[1],"ordinary")) ordinary_case();
    else if (!strcmp(argv[1],"ordered-off")) ordered_off_case();
    else return 2;
    puts("PASS: CPU surface fallback uses ordered current-layout barrier only on its async surface path");
}
'''


def build_and_run(directory: Path, code: str, mode: str, env: dict[str, str], expect_failure: bool = False) -> None:
    src = directory / "ordered.c"
    binary = directory / "ordered"
    src.write_text(code)
    includes = ["-I" + str(root / "plugin"), "-I" + os.environ.get("VULKAN_HEADERS", str(Path.home() / "Vulkan-Headers/include"))]
    subprocess.run(["xcrun", "clang", "-std=c11", *includes, str(src), "-o", str(binary)], check=True)
    run_env = os.environ.copy()
    for key, value in env.items():
        if value is None:
            run_env.pop(key, None)
        else:
            run_env[key] = value
    result = subprocess.run([str(binary), mode], env=run_env, text=True, capture_output=True, timeout=10)
    if expect_failure:
        if result.returncode == 0:
            raise AssertionError("negative control unexpectedly passed")
    elif result.returncode:
        raise AssertionError(f"{result.args} failed ({result.returncode})\n{result.stderr}")
    else:
        print(result.stdout, end="")


with tempfile.TemporaryDirectory() as td:
    directory = Path(td)
    code = fixture + ordered + helper + checks
    build_and_run(directory, code, "async", {"NVMTL_SURFIN_ORDERED": None})
    build_and_run(directory, code, "same", {"NVMTL_SURFIN_ORDERED": None})
    build_and_run(directory, code, "ordinary", {"NVMTL_SURFIN_ORDERED": None})
    build_and_run(directory, code, "ordered-off", {"NVMTL_SURFIN_ORDERED": "0"})

    broken = ordered.replace("(VkImageLayout)img->layout, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL",
                             "VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL", 1)
    broken = broken.replace("VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT",
                            "VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT", 1)
    build_and_run(directory, fixture + broken + helper + checks, "async",
                  {"NVMTL_SURFIN_ORDERED": None}, expect_failure=True)
