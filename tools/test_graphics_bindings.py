#!/usr/bin/env python3
"""Exercise production graphics binding elision, including every cache input."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root/'plugin/nvmtl_vk.c').read_text()
def section(start, end):
    i = source.index(start)
    return source[i:source.index(end, i)]

sets = section('static void nvmtl_bind_graphics_sets(', '\nvoid nvmtl_vk_cmd_bind_set(')
pipeline = section('void nvmtl_vk_cmd_bind_pipeline(', '\nint nvmtl_vk_cmd_set_topology(')
assert 'memset(out, 0, sizeof *out);' in section('int nvmtl_vk_cmd_begin(', '\nstatic pthread_mutex_t g_rpv_lock')
assert source.count('pvkCmdBindPipeline(c->cb, VK_PIPELINE_BIND_POINT_GRAPHICS,') == 1
assert 'if (p->pipe) nvmtl_vk_cmd_bind_pipeline(c, p);' in source

fixture = r'''
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include <mach/mach.h>
#include <vulkan/vulkan.h>
#include "nvmtl_vk.h"
static unsigned descriptor_calls, pipeline_calls;
static void pvkCmdBindDescriptorSets(VkCommandBuffer cb,VkPipelineBindPoint point,VkPipelineLayout layout,
        unsigned first,unsigned count,const VkDescriptorSet *sets,unsigned ndyn,const unsigned *offsets) {
    assert(point==VK_PIPELINE_BIND_POINT_GRAPHICS && first==0 && count<=3 && ndyn<=2);
    descriptor_calls++;
}
static void pvkCmdBindPipeline(VkCommandBuffer cb,VkPipelineBindPoint point,VkPipeline pipeline) {
    assert(point==VK_PIPELINE_BIND_POINT_GRAPHICS); pipeline_calls++;
}
''' + sets + '\n' + pipeline + r'''
int main(void) {
    nvk_cmdbuf c={0};
    VkDescriptorSet sets[3]={(VkDescriptorSet)10,(VkDescriptorSet)20,(VkDescriptorSet)30};
    unsigned offsets[2]={0,256}; VkPipelineLayout layout=(VkPipelineLayout)40;
    nvmtl_bind_graphics_sets(&c,layout,2,sets,2,offsets); assert(descriptor_calls==1);
    for(unsigned i=0;i<10000;i++) nvmtl_bind_graphics_sets(&c,layout,2,sets,2,offsets);
    assert(descriptor_calls==1);
    offsets[0]=256; nvmtl_bind_graphics_sets(&c,layout,2,sets,2,offsets); assert(descriptor_calls==2);
    offsets[1]=512; nvmtl_bind_graphics_sets(&c,layout,2,sets,2,offsets); assert(descriptor_calls==3);
    sets[1]=(VkDescriptorSet)21; nvmtl_bind_graphics_sets(&c,layout,2,sets,2,offsets); assert(descriptor_calls==4);
    layout=(VkPipelineLayout)41; nvmtl_bind_graphics_sets(&c,layout,2,sets,2,offsets); assert(descriptor_calls==5);
    nvmtl_bind_graphics_sets(&c,layout,3,sets,2,offsets); assert(descriptor_calls==6);
    sets[2]=(VkDescriptorSet)31; nvmtl_bind_graphics_sets(&c,layout,3,sets,2,offsets); assert(descriptor_calls==7);
    nvmtl_bind_graphics_sets(&c,layout,3,sets,0,NULL); assert(descriptor_calls==8);
    offsets[0]=768; nvmtl_bind_graphics_sets(&c,layout,3,sets,0,offsets); assert(descriptor_calls==8);
    nvmtl_bind_graphics_sets(&c,layout,3,sets,2,offsets); assert(descriptor_calls==9);
    nvmtl_bind_graphics_sets(&c,layout,2,sets,2,offsets); assert(descriptor_calls==10);
    nvk_pipeline a={.pipe=(void *)50},alias={.pipe=(void *)50},b={.pipe=(void *)51};
    nvmtl_vk_cmd_bind_pipeline(&c,&a); assert(pipeline_calls==1);
    for(unsigned i=0;i<10000;i++) nvmtl_vk_cmd_bind_pipeline(&c,&a);
    nvmtl_vk_cmd_bind_pipeline(&c,&alias); assert(pipeline_calls==1);
    nvmtl_vk_cmd_bind_pipeline(&c,&b); assert(pipeline_calls==2);
    nvmtl_vk_cmd_bind_pipeline(&c,&a); assert(pipeline_calls==3);
    // Fresh command recording cannot reuse state from a recycled pool or an earlier event segment.
    memset(&c,0,sizeof c);
    nvmtl_vk_cmd_bind_pipeline(&c,&a); assert(pipeline_calls==4);
    nvmtl_bind_graphics_sets(&c,layout,2,sets,2,offsets); assert(descriptor_calls==11);
    nvk_cmdbuf independent={0};
    nvmtl_bind_graphics_sets(&independent,layout,2,sets,2,offsets); assert(descriptor_calls==12);
    puts("PASS: identical bindings skipped; resource sets, heap, layout, both offsets/counts, pipeline changes, new recordings and independent buffers rebind");
}
'''
with tempfile.TemporaryDirectory(prefix='nvmtl-graphics-bindings-') as tmp:
    path=pathlib.Path(tmp)
    (path/'test.c').write_text(fixture)
    subprocess.run(['xcrun','clang','-O2',str(path/'test.c'),'-I'+str(root/'plugin'),
                    '-I'+str(pathlib.Path.home()/'Vulkan-Headers/include'),'-o',str(path/'test')],check=True)
    subprocess.run([str(path/'test')],check=True)
