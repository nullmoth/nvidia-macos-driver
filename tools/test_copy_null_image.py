#!/usr/bin/env python3
"""A buffer<->texture copy to a texture with no VkImage is refused before any Vulkan call.

Logic Pro 12.3.1 on 2.6.5 (plugin 78BE873A) crashed opening the Dolby Atmos mixer: RealityKit's TextureManager uploaded
a texture through copyFromBuffer:toTexture:, the layout barrier carried a null VkImage, and NVK read 0x520 from it in
nvk_CmdPipelineBarrier2. The production copy functions are compiled with Vulkan dispatch stubbed; an optional argument
checks another nvmtl_vk.c (the 2.6.5 one dispatches the null image and fails the first arms)."""
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source_path = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else root / 'plugin/nvmtl_vk.c'
source = source_path.read_text()


def function(signature, required=True):
    if signature not in source:
        if required:
            raise SystemExit(f'missing {signature}')
        return ''
    start = source.index(signature)
    while source.index(';', start) < source.index('{', start):
        start = source.index(signature, start + len(signature))
    cursor = source.index('{', start) + 1
    depth = 1
    while depth:
        depth += (source[cursor] == '{') - (source[cursor] == '}')
        cursor += 1
    return source[start:cursor] + '\n'


fixture = r'''
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdatomic.h>
#include <mach/mach.h>
#include <vulkan/vulkan.h>
#include "nvmtl_vk.h"
static unsigned logs, barriers, copies, errors, checks;
#define nvlog(...) (logs++)
static uint32_t g_copy_bpp_override, g_copy_aspect_override;
void nvmtl_vk_cmd_barrier(nvk_cmdbuf *c) {}
static void img_to(nvk_cmdbuf *c, nvk_image *i, VkImageLayout l,
                   VkAccessFlags a, VkAccessFlags b, VkPipelineStageFlags s, VkPipelineStageFlags t) {
    barriers++; if (!i->img) { fprintf(stderr, "FAIL: a null VkImage reached the layout barrier\n"); errors++; } i->layout = l; }
static void pvkCmdCopyBufferToImage(VkCommandBuffer cb, VkBuffer b, VkImage i, VkImageLayout l, uint32_t n, const VkBufferImageCopy *r) { copies++; }
static void pvkCmdCopyImageToBuffer(VkCommandBuffer cb, VkImage i, VkImageLayout l, VkBuffer b, uint32_t n, const VkBufferImageCopy *r) { copies++; }
static void check(int ok, const char *name) { checks++; printf("%s %s\n", ok ? "ok  " : "FAIL", name); if (!ok) errors++; }
''' + '\n'.join(function(sig, req) for sig, req in [
    ('int nvmtl_vk_format_block(', True),
    ('static VkImageAspectFlags nvmtl_barrier_aspect(', True),
    ('static VkImageAspectFlags nvmtl_copy_aspect(', True),
    ('static inline uint32_t nvmtl_lvl(', True),
    ('static inline uint32_t nvmtl_blocks(', True),
    ('static int nvmtl_copy_has_image(', False),
    ('int nvmtl_vk_cmd_copy_buffer_to_image_level_layer(', True),
    ('int nvmtl_vk_cmd_copy_image_to_buffer_level_layer(', True),
]) + r'''
int main(void) {
    nvk_cmdbuf c = {0};
    nvk_buffer b = {0}; b.buf = (void *)3; b.size = 1 << 20;
    nvk_image none = { .img = NULL, .w = 64, .h = 64, .mips = 1, .layers = 1, .bpp = 4, .fmt = VK_FORMAT_R8G8B8A8_UNORM, .mtl_type = 2 };
    unsigned before = barriers + copies;
    int rc = nvmtl_vk_cmd_copy_buffer_to_image_level_layer(&c, &b, 0, 256, &none, 0, 0, 64, 64, 0, 0);
    check(rc == -1, "buffer -> texture with no VkImage is refused");
    check(barriers + copies == before, "and nothing reaches Vulkan (no barrier, no copy)");
    check(logs > 0, "and the refusal is logged");
    rc = nvmtl_vk_cmd_copy_image_to_buffer_level_layer(&c, &none, 0, 0, 64, 64, &b, 0, 256, 0, 0);
    check(rc == -1 && barriers + copies == before, "texture with no VkImage -> buffer is refused before Vulkan");
    nvk_image good = none; good.img = (void *)1;
    rc = nvmtl_vk_cmd_copy_buffer_to_image_level_layer(&c, &b, 0, 256, &good, 0, 0, 64, 64, 0, 0);
    check(rc == 0 && copies == 1 && barriers == 2, "negative control: a texture with a VkImage still copies (2 barriers, 1 copy)");
    rc = nvmtl_vk_cmd_copy_image_to_buffer_level_layer(&c, &good, 0, 0, 64, 64, &b, 0, 256, 0, 0);
    check(rc == 0 && copies == 2, "negative control: and reads back");
    for (int i = 0; i < 40; i++) nvmtl_vk_cmd_copy_buffer_to_image_level_layer(&c, &b, 0, 256, &none, 0, 0, 64, 64, 0, 0);
    check(logs <= 16, "a per-frame refusal is logged at most 16 times");
    printf("%s: %u checks, %u failures\n", errors ? "FAIL" : "PASS", checks, errors);
    return errors ? 1 : 0;
}
'''

with tempfile.TemporaryDirectory(prefix='nvmtl-copy-null-image-') as temporary:
    path = pathlib.Path(temporary)
    (path / 'test.c').write_text(fixture)
    r = subprocess.run(['xcrun', 'clang', '-O1', '-g', '-fsanitize=address,undefined', str(path / 'test.c'),
                        '-I' + str(root / 'plugin'), '-I' + str(pathlib.Path.home() / 'Vulkan-Headers/include'),
                        '-o', str(path / 'test')], capture_output=True, text=True, timeout=300)
    if r.returncode:
        print(r.stderr[-2000:])
        sys.exit(1)
    sys.exit(subprocess.run([str(path / 'test')], timeout=60).returncode)
