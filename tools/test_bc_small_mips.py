#!/usr/bin/env python3
"""Exercise production compressed-image copy bounds with Vulkan dispatch stubbed.

An optional source path permits checking an older revision with the same cases.
This does not load a driver or perform a GPU copy.
"""
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source_path = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else root/'plugin/nvmtl_vk.c'
source = source_path.read_text()


def function(signature):
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
#define nvlog(...) ((void)0)
static unsigned calls, errors, checks;
static VkImageCopy region;
void nvmtl_vk_cmd_barrier(nvk_cmdbuf *c) {}
static void img_to(nvk_cmdbuf *c, nvk_image *i, VkImageLayout l,
                  VkAccessFlags a, VkAccessFlags b, VkPipelineStageFlags s, VkPipelineStageFlags t) { i->layout=l; }
static void pvkCmdCopyImage(VkCommandBuffer cb, VkImage a, VkImageLayout al, VkImage b,
                          VkImageLayout bl, uint32_t n, const VkImageCopy *regions) {
    calls++; region=*regions;
}
static void check(int ok, const char *name) {
    checks++;
    if (!ok && errors++ < 12) fprintf(stderr, "FAIL: %s\n", name);
}
''' + '\n'.join(function(sig) for sig in [
    'int nvmtl_vk_format_block(',
    'static VkImageAspectFlags nvmtl_barrier_aspect(',
    'static inline uint32_t nvmtl_lvl(',
    'int nvmtl_vk_cmd_copy_image_sub3(',
]) + r'''
static int copy(nvk_image *s, nvk_image *d, unsigned sl, unsigned dl,
                unsigned sx, unsigned sy, unsigned dx, unsigned dy, unsigned w, unsigned h) {
    nvk_cmdbuf c={0};
    return nvmtl_vk_cmd_copy_image_sub3(&c,s,sl,0,sx,sy,0,d,dl,0,dx,dy,0,w,h,1);
}
static void known_cases(void) {
    for (unsigned d=1;d<=4;d++) {
        nvk_image s={.img=(void *)1,.w=d,.h=d,.mips=1,.fmt=VK_FORMAT_BC3_UNORM_BLOCK,.layers=1};
        nvk_image t=s; t.img=(void *)2;
        unsigned before=calls;
        check(copy(&s,&t,0,0,0,0,0,0,4,4)==0,"same-size tiny mip accepted");
        check(calls==before+1 && region.extent.width==d && region.extent.height==d,"tiny mip ends at actual source edge");
        before=calls;
        check(copy(&s,&t,0,0,0,0,0,0,8,4)==-1 && calls==before,"oversized rounded request refused before dispatch");
    }
    nvk_image s={.img=(void *)1,.w=4,.h=4,.mips=1,.fmt=VK_FORMAT_BC1_RGBA_UNORM_BLOCK,.layers=1};
    nvk_image t=s; t.img=(void *)2; t.w=t.h=2;
    unsigned before=calls;
    check(copy(&s,&t,0,0,0,0,0,0,4,4)==-1 && calls==before,"4x4 to 2x2 must not dispatch a partial source block");
    t.w=4;
    check(copy(&s,&t,0,0,0,0,0,0,4,4)==-1 && calls==before,"cropped height must not split a source block");
    t.w=2;t.h=4;
    check(copy(&s,&t,0,0,0,0,0,0,4,4)==-1 && calls==before,"cropped width must not split a source block");
    s.w=s.h=2;t.w=t.h=4;
    check(copy(&s,&t,0,0,0,0,0,0,2,2)==0,"source-edge copy to a larger destination is retained");
    s.w=s.h=t.w=t.h=16;s.mips=t.mips=5;
    for (unsigned level=2;level<=4;level++) {
        check(copy(&s,&t,level,level,0,0,0,0,4,4)==0,"copy uses selected mip dimensions");
        check(region.extent.width==(16u>>level) && region.srcSubresource.mipLevel==level,"selected mip dispatch extent");
    }
    s.mips=t.mips=1;s.w=s.h=t.w=t.h=4;s.fmt=t.fmt=VK_FORMAT_R8G8B8A8_UNORM;
    check(copy(&s,&t,0,0,1,1,1,1,2,2)==0,"uncompressed regions retain texel alignment");
}
static void small_region_matrix(uint32_t format, int vertical) {
    for (unsigned sd=1;sd<=9;sd++) for (unsigned dd=1;dd<=9;dd++)
    for (unsigned so=0;so<8;so++) for (unsigned doff=0;doff<8;doff++)
    for (unsigned size=0;size<=12;size++) {
        nvk_image s={.img=(void *)1,.w=4,.h=4,.mips=1,.fmt=format,.layers=1};
        nvk_image d=s; d.img=(void *)2;
        if (vertical) { s.h=sd;d.h=dd; } else { s.w=sd;d.w=dd; }
        unsigned before=calls;
        int rc=copy(&s,&d,0,0,vertical?0:so,vertical?so:0,vertical?0:doff,vertical?doff:0,
                    vertical?4:size,vertical?size:4);
        if (rc) {
            check(calls==before,"refused region never reaches Vulkan dispatch");
            continue;
        }
        uint32_t extent=vertical?region.extent.height:region.extent.width;
        check(calls==before+1,"accepted region dispatched once");
        check(extent && (uint64_t)so+extent<=sd && (uint64_t)doff+extent<=dd,"dispatched region within both mip bounds");
        check(so%4==0 && doff%4==0,"source and destination offsets are block-aligned");
        check(extent%4==0 || (uint64_t)so+extent==sd,"partial block reaches source mip edge");
    }
}
int main(void) {
    known_cases();
    small_region_matrix(VK_FORMAT_BC1_RGBA_UNORM_BLOCK,0);
    small_region_matrix(VK_FORMAT_BC1_RGBA_UNORM_BLOCK,1);
    small_region_matrix(VK_FORMAT_BC3_UNORM_BLOCK,0);
    small_region_matrix(VK_FORMAT_BC3_UNORM_BLOCK,1);
    printf("%s: %u checks, %u failures; BC1/BC3 region matrix, cropped dimensions and mip edges; Vulkan dispatch stubbed\n",
           errors?"FAIL":"PASS",checks,errors);
    return errors?1:0;
}
'''

with tempfile.TemporaryDirectory(prefix='nvmtl-bc-small-mips-') as temporary:
    path = pathlib.Path(temporary)
    (path/'test.c').write_text(fixture)
    subprocess.run(['xcrun','clang','-O1','-g','-fsanitize=address,undefined',str(path/'test.c'),
                    '-I'+str(root/'plugin'),'-I'+str(pathlib.Path.home()/'Vulkan-Headers/include'),
                    '-o',str(path/'test')],check=True)
    subprocess.run([str(path/'test')],check=True)
